use super::{channels, failure, finished, received, signal};
use rusterisk_api::ChannelId;
use rusterisk_config::Codec;
use rusterisk_core::PeerMediaDirection;
use rusterisk_media::MediaError;
use rusterisk_server::{
    media::{MediaHub, MediaNotice},
    rtp::SocketPair,
    webrtc::{WebRtcOffer, WebRtcSession},
    CallKey,
};
use rusterisk_test_support::Wait;
use std::{
    collections::BTreeSet,
    net::Ipv4Addr,
    time::{Duration, Instant},
};
use tokio::sync::watch;

fn pair() -> Result<(WebRtcSession, WebRtcSession, std::net::SocketAddr), MediaError> {
    let server = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let client = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let server_address = server.local_addr().map_err(failure)?;
    let client_address = client.local_addr().map_err(failure)?;
    let offer = WebRtcOffer::new(server, server_address, &[Codec::Pcmu], Instant::now())?;
    let (client, answer) = WebRtcSession::answer(
        client,
        client_address,
        offer.description(),
        &[Codec::Pcmu],
        Instant::now(),
    )?;
    Ok((offer.accept_answer(&answer)?, client, server_address))
}

#[tokio::test]
async fn bridge_reports_connection_only_after_received_audio_and_releases_owned_sessions(
) -> Result<(), MediaError> {
    let (left_server, left_client, left_address) = pair()?;
    let (right_server, right_client, right_address) = pair()?;
    let keys = [
        CallKey {
            id: ChannelId::try_from("left".to_owned()).map_err(failure)?,
            generation: 1,
        },
        CallKey {
            id: ChannelId::try_from("right".to_owned()).map_err(failure)?,
            generation: 2,
        },
    ];
    let (media, hub, mut notices) = MediaHub::new(2);
    let (stop, stopped) = watch::channel(false);
    let hub = tokio::spawn(hub.run(stopped));
    let (left_channels, mut left_app) = channels();
    let (right_channels, mut right_app) = channels();
    let left = tokio::spawn(super::webrtc_driver::run(left_client, left_channels));
    let right = tokio::spawn(super::webrtc_driver::run(right_client, right_channels));
    let result = Wait::Ready.bounded(async {
        media.attach_webrtc(keys[0].clone(), left_server, PeerMediaDirection::SendReceive).await?;
        media.attach_webrtc(keys[1].clone(), right_server, PeerMediaDirection::SendReceive).await?;
        media.bridge(1, keys.to_vec())?;
        left_app.incoming.recv().await.ok_or(MediaError::UnknownStream)?;
        right_app.incoming.recv().await.ok_or(MediaError::UnknownStream)?;
        assert!(notices.try_recv().is_err());
        let mut clock = tokio::time::interval(Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut sent = 0;
        let mut audio = [false; 2];
        let mut connected = BTreeSet::new();
        while sent < 30 || !audio.iter().all(|received| *received) || connected.len() < 2 {
            tokio::select! {
                _instant = clock.tick(), if sent < 30 => {
                    left_app.outgoing.try_send(signal(440.0, sent)?).map_err(failure)?;
                    right_app.outgoing.try_send(signal(660.0, sent)?).map_err(failure)?;
                    sent += 1;
                }
                frame = left_app.incoming.recv() => audio[0] |= received(&frame.ok_or(MediaError::UnknownStream)?, 660.0, 440.0),
                frame = right_app.incoming.recv() => audio[1] |= received(&frame.ok_or(MediaError::UnknownStream)?, 440.0, 660.0),
                notice = notices.recv() => match notice.ok_or(MediaError::UnknownStream)? {
                    MediaNotice::Connected {key} => { connected.insert(key); }
                    MediaNotice::Stopped { result, .. } => { result?; return Err(MediaError::UnknownStream); }
                    MediaNotice::Released {..} | MediaNotice::Answer {..} | MediaNotice::Dtmf {..} | MediaNotice::DtmfFinished {..} | MediaNotice::Attached {..} | MediaNotice::DirectionApplied {..} => return Err(MediaError::Unsupported),
                }
            }
        }
        assert!(keys.iter().all(|key| connected.contains(key)));
        for key in &keys { media.release(key.clone())?; }
        Ok::<(), MediaError>(())
    }).await.map_err(failure);
    left_app.stop.send_replace(true);
    right_app.stop.send_replace(true);
    let left = finished(left).await;
    let right = finished(right).await;
    stop.send_replace(true);
    let hub = hub.await.map_err(failure)?;
    result??;
    left?;
    right?;
    hub?;
    for address in [left_address, right_address] {
        let rebound =
            SocketPair::bind(address.ip(), address.port(), address.port() + 1).map_err(failure)?;
        assert_eq!(rebound.local_addr().map_err(failure)?, address);
    }
    Ok(())
}
