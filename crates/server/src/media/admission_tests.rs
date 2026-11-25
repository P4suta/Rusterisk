use super::*;
use rusterisk_api::ChannelId;
use rusterisk_media::AudioFormat;
use std::net::Ipv4Addr;
use tokio::net::{TcpListener, TcpStream};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

fn key() -> Result<CallKey, MediaError> {
    Ok(CallKey {
        id: ChannelId::try_from("existing".to_owned()).map_err(failure)?,
        generation: 19,
    })
}

#[tokio::test]
async fn measurement_reply_preserves_the_capture_time_and_counts() -> Result<(), MediaError> {
    let (_handle, mut hub, _notices) = MediaHub::new(4);
    let key = key()?;
    let (sender, reader) = watch::channel(crate::rtp::Measurements::default());
    let (mut port, _channels) =
        Port::new(PeerMediaDirection::SendReceive, DigitSupport::Unsupported);
    port.measurements = Some(reader);
    hub.install_link(key.clone(), port, Gate::Open)?;
    sender.send_modify(|sample| sample.local.rxcount = 7);
    let (reply, response) = oneshot::channel();
    let before = crate::utc_timestamps::now_millis();
    hub.command(Command::Measurements { key, reply })?;
    let after = crate::utc_timestamps::now_millis();
    sender.send_modify(|sample| sample.local.rxcount = 8);
    hub.close().await?;
    let (captured, sample) = response
        .await
        .map_err(failure)?
        .ok_or(MediaError::UnknownStream)?;
    assert!((before..=after).contains(&captured));
    assert_eq!(sample.local.rxcount, 7);
    Ok(())
}

fn datagram(hub: &mut MediaHub, key: CallKey, peer: &SocketPair) -> Result<(), MediaError> {
    hub.attach(
        key,
        SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?,
        RtpSession {
            remote: peer.local_addr().map_err(failure)?,
            format: AudioFormat::Pcmu,
            payload_type: 0,
            telephone: None,
        },
        PeerMediaDirection::SendReceive,
        AttachmentMode::Answered,
    )
}

async fn stream() -> Result<(TcpStream, TcpStream), MediaError> {
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
    let (peer, (socket, _)) = tokio::try_join!(
        TcpStream::connect(listener.local_addr().map_err(failure)?),
        listener.accept()
    )
    .map_err(failure)?;
    Ok((peer, socket))
}

async fn rejected(
    hub: &mut MediaHub,
    key: &CallKey,
    result: Result<(), MediaError>,
) -> Result<(), MediaError> {
    assert!(matches!(result, Err(MediaError::InvalidRoute)));
    let (reply, response) = oneshot::channel();
    drop(response);
    hub.complete_attachment(key, result, reply)?;
    let preserved = hub.ports.get(key).is_some_and(|port| !*port.stop.borrow());
    hub.close().await?;
    assert!(
        preserved,
        "a rejected attachment cannot release an existing link when its reply receiver closes"
    );
    Ok(())
}

#[tokio::test]
async fn early_attachment_closes_conversation_and_generated_audio_before_publication(
) -> Result<(), MediaError> {
    let (_handle, mut hub, _notices) = MediaHub::new(4);
    let early = key()?;
    let other = CallKey {
        id: ChannelId::try_from("peer".to_owned()).map_err(failure)?,
        generation: 20,
    };
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let result = (|| {
        hub.attach(
            early.clone(),
            SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?,
            RtpSession {
                remote: peer.local_addr().map_err(failure)?,
                format: AudioFormat::Pcmu,
                payload_type: 0,
                telephone: None,
            },
            PeerMediaDirection::SendReceive,
            AttachmentMode::Early,
        )?;
        datagram(&mut hub, other.clone(), &peer)?;
        hub.mixer.bridge(1, &[early.clone(), other.clone()])?;
        hub.mixer.push(&early, Frame::new(&[4000; 160], 8000)?)?;
        hub.mixer.push(&other, Frame::new(&[-7000; 160], 8000)?)?;
        let audio = hub.mixer.tick_with_playback(&[(
            AudioTarget::Channel(early.clone()),
            Frame::new(&[1234; 960], 48000)?,
        )])?;
        if !hub
            .ports
            .get(&early)
            .is_some_and(|port| port.generated_audio_gate == Gate::Closed)
            || audio.channels.len() != 2
            || audio
                .channels
                .iter()
                .any(|(_, frame)| frame.samples().iter().any(|sample| *sample != 0))
        {
            return Err(MediaError::InvalidRoute);
        }
        Ok(())
    })();
    let closed = hub.close().await;
    match (result, closed) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(MediaError::Transport(format!(
            "{primary}; early admission cleanup failed: {cleanup}"
        ))),
    }
}

#[tokio::test]
async fn cancelled_duplicate_datagram_admission_keeps_the_existing_link() -> Result<(), MediaError>
{
    let (_handle, mut hub, _notices) = MediaHub::new(4);
    let key = key()?;
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    datagram(&mut hub, key.clone(), &peer)?;
    let result = datagram(&mut hub, key.clone(), &peer);
    rejected(&mut hub, &key, result).await
}

#[tokio::test]
async fn cancelled_duplicate_stream_admission_keeps_the_existing_link() -> Result<(), MediaError> {
    let (_handle, mut hub, _notices) = MediaHub::new(4);
    let key = key()?;
    let (_original_peer, socket) = stream().await?;
    hub.attach_audiosocket(key.clone(), socket, 8000)?;
    let (_duplicate_peer, socket) = stream().await?;
    let result = hub.attach_audiosocket(key.clone(), socket, 8000);
    rejected(&mut hub, &key, result).await
}

#[tokio::test]
async fn cancelled_successful_admission_releases_the_new_link() -> Result<(), MediaError> {
    let (_handle, mut hub, _notices) = MediaHub::new(4);
    let key = key()?;
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    datagram(&mut hub, key.clone(), &peer)?;
    let (reply, response) = oneshot::channel();
    drop(response);
    hub.complete_attachment(&key, Ok(()), reply)?;
    let removed = !hub.ports.contains_key(&key);
    hub.close().await?;
    assert!(removed);
    Ok(())
}

fn browser() -> Result<crate::webrtc::WebRtcSession, MediaError> {
    let offering = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let answering = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let now = std::time::Instant::now();
    let codecs = [rusterisk_config::Codec::Pcmu];
    let offer = crate::webrtc::WebRtcOffer::new(
        offering,
        answering.local_addr().map_err(failure)?,
        &codecs,
        now,
    )?;
    let local = answering.local_addr().map_err(failure)?;
    let (session, _answer) =
        crate::webrtc::WebRtcSession::answer(answering, local, offer.description(), &codecs, now)?;
    Ok(session)
}

// Runs the next queued command and then every attachment that still waits.
fn settle(hub: &mut MediaHub) -> Result<(), MediaError> {
    let command = hub.commands.try_recv().map_err(failure)?;
    hub.ordered(command)?;
    while let Ok(attachment) = hub.webrtc.try_recv() {
        hub.webrtc_attachment(attachment)?;
    }
    Ok(())
}

#[tokio::test]
async fn a_bridge_command_finds_the_webrtc_attachment_that_was_queued_before_it(
) -> Result<(), MediaError> {
    let (handle, mut hub, mut notices) = MediaHub::new(4);
    let key = key()?;
    let _receipt = handle.queue_webrtc_attachment(
        key.clone(),
        browser()?,
        PeerMediaDirection::SendReceive,
        AttachmentMode::Answered,
    )?;
    handle.bridge(7, Vec::from([key.clone()]))?;
    settle(&mut hub)?;
    let attached = hub.ports.contains_key(&key);
    let stopped = notices.try_recv().is_ok();
    hub.close().await?;
    assert!(attached);
    assert!(!stopped);
    Ok(())
}

#[tokio::test]
async fn a_release_ends_the_webrtc_attachment_that_was_queued_before_it() -> Result<(), MediaError>
{
    let (handle, mut hub, _notices) = MediaHub::new(4);
    let key = key()?;
    let _receipt = handle.queue_webrtc_attachment(
        key.clone(),
        browser()?,
        PeerMediaDirection::SendReceive,
        AttachmentMode::Answered,
    )?;
    handle.release(key.clone())?;
    settle(&mut hub)?;
    let remaining = hub.ports.contains_key(&key);
    hub.close().await?;
    assert!(!remaining);
    Ok(())
}
