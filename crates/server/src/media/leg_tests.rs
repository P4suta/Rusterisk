use super::*;
use rusterisk_api::ChannelId;
use rusterisk_core::{DtmfDigit, DtmfOutbox, DtmfTiming};
use rusterisk_media::{AudioFormat, ControlKind, RtpPacket};
use std::net::Ipv4Addr;
use std::time::Duration;

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

fn key(name: &str) -> Result<CallKey, MediaError> {
    Ok(CallKey {
        id: ChannelId::try_from(name.to_owned()).map_err(failure)?,
        generation: 1,
    })
}

// Attaches one protocol leg, with telephone events when the session offers them.
fn leg(
    hub: &mut MediaHub,
    key: &CallKey,
    telephone: Option<TelephoneFormat>,
) -> Result<SocketPair, MediaError> {
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    hub.attach(
        key.clone(),
        SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?,
        RtpSession {
            remote: peer.local_addr().map_err(failure)?,
            format: AudioFormat::Pcmu,
            payload_type: 0,
            telephone,
        },
        PeerMediaDirection::SendReceive,
        AttachmentMode::Answered,
    )?;
    Ok(peer)
}

fn read(hub: &MediaHub, key: &CallKey) -> Result<u64, MediaError> {
    Ok(hub
        .legs
        .get(key)
        .ok_or(MediaError::UnknownStream)?
        .measurements()
        .local
        .rxcount)
}

// A peer floods a leg, and each turn reads one reorder window of its datagrams.
#[tokio::test]
async fn a_leg_reads_at_most_one_window_of_datagrams_in_a_turn() -> Result<(), MediaError> {
    let (_handle, mut hub, _notices) = MediaHub::new(4);
    let flooded = key("flooded")?;
    let peer = leg(&mut hub, &flooded, None)?;
    let local = hub
        .legs
        .get(&flooded)
        .ok_or(MediaError::UnknownStream)?
        .local_addr()?;
    for sequence in 0..40_u16 {
        let packet = RtpPacket::new(0, sequence, u32::from(sequence) * 160, 7, &[0x55; 160])?;
        peer.data()
            .send_to(&packet.encode()?, local)
            .await
            .map_err(failure)?;
    }
    // A loopback datagram can reach its socket after the sender returns.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut counts = Vec::with_capacity(3);
    for _ in 0..3 {
        hub.tick()?;
        counts.push(read(&hub, &flooded)?);
    }
    hub.close().await?;
    assert_eq!(counts, [16, 32, 40]);
    Ok(())
}

// Signaling sends a digit sequence to a leg that the media task owns.
#[tokio::test]
async fn an_owned_leg_sends_digits_and_reports_their_finish() -> Result<(), MediaError> {
    let (_handle, mut hub, mut notices) = MediaHub::new(4);
    let dialing = key("dialing")?;
    let format = TelephoneFormat::new(101, 8000)?;
    let peer = leg(&mut hub, &dialing, Some(format))?;
    let timing = DtmfTiming::new(0, 20, 60, 0).map_err(|_| MediaError::InvalidRoute)?;
    let command = DtmfOutbox::new(u16::MAX)
        .and_then(|mut outbox| outbox.admit(&[DtmfDigit::Five], timing))
        .map_err(|_| MediaError::InvalidRoute)?;
    let token = command.token;
    let (reply, mut response) = oneshot::channel();
    hub.command(Command::Digits {
        key: dialing.clone(),
        command: RtpDigitCommand::Sequence(command),
        reply: Some(reply),
    })?;
    assert!(matches!(response.try_recv(), Ok(Ok(()))));
    let mut finished = false;
    for _ in 0..50 {
        hub.tick()?;
        while let Ok(notice) = notices.try_recv() {
            finished |= matches!(notice, MediaNotice::DtmfFinished { key, token: done } if key == dialing && done == token);
        }
        if finished {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The leg also sends the mixer's audio, so the peer looks for the first telephone event.
    let mut bytes = [0; 1501];
    let mut events = 0;
    while let Ok(Ok((size, _))) = tokio::time::timeout(
        Duration::from_millis(100),
        peer.data().recv_from(&mut bytes),
    )
    .await
    {
        events += usize::from(RtpPacket::parse(&bytes[..size])?.payload_type() == 101);
    }
    hub.command(Command::Release { key: dialing })?;
    let (size, _) =
        tokio::time::timeout(Duration::from_secs(1), peer.control().recv_from(&mut bytes))
            .await
            .map_err(failure)?
            .map_err(failure)?;
    let goodbye = rusterisk_media::inspect_rtcp(&bytes[..size])?;
    hub.close().await?;
    assert!(finished, "the leg never reported the finished digits");
    assert!(events > 0, "the peer received no telephone event");
    assert_eq!(goodbye.last(), Some(&ControlKind::Goodbye));
    Ok(())
}
