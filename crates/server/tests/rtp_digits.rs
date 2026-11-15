use rusterisk_core::{DtmfDigit, DtmfTone};
use rusterisk_media::{
    mulaw_decode, AudioFormat, ControlKind, Frame, MediaError, OpusCodec, RtpPacket,
    TelephoneEvent, TelephoneFormat,
};
use rusterisk_server::rtp::{RtpChannels, RtpDigitCommand, RtpLink, RtpSession, SocketPair};
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

#[tokio::test]
async fn outgoing_digits_share_or_separate_clocks_and_close_every_reported_source(
) -> Result<(), MediaError> {
    for (format, payload_type) in [(AudioFormat::Pcmu, 0), (AudioFormat::Opus, 111)] {
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let peer = SocketPair::bind(ip, 0, 0).map_err(failure)?;
        let sockets = SocketPair::bind(ip, 0, 0).map_err(failure)?;
        let (incoming, _audio) = mpsc::channel(3);
        let (digits, _digits) = mpsc::channel(16);
        let (outgoing, output) = mpsc::channel(3);
        let (tones, tone_input) = mpsc::channel(16);
        let (completed, _completed) = mpsc::channel(8);
        let (stop, stopped) = watch::channel(false);
        let (_direction, direction) =
            watch::channel(rusterisk_core::PeerMediaDirection::SendReceive);
        let link = RtpLink::new(
            sockets,
            RtpSession {
                remote: peer.local_addr().map_err(failure)?,
                format,
                payload_type,
                telephone: Some(TelephoneFormat::new(101, 8000)?),
            },
            RtpChannels {
                incoming,
                digits,
                outgoing: output,
                commands: tone_input,
                completed,
                stop: stopped,
                direction,
            },
        )?;
        let task = tokio::spawn(link.run());
        let mut counts = BTreeMap::<u32, u32>::new();
        let result = async {
            outgoing
                .send(Frame::new(&[6000; 160], 8000)?)
                .await
                .map_err(failure)?;
            tones
                .send(RtpDigitCommand::Tone(DtmfTone {
                    digit: DtmfDigit::Hash,
                    duration_ms: 60,
                }))
                .await
                .map_err(failure)?;
            tokio::time::timeout(Duration::from_secs(2), async {
                let mut bytes = [0; 1501];
                let mut audio_source = None;
                let mut event_source = None;
                let mut ends = 0;
                for _ in 0..32 {
                    let (size, _) = peer.data().recv_from(&mut bytes).await.map_err(failure)?;
                    let packet = RtpPacket::parse(&bytes[..size])?;
                    *counts.entry(packet.ssrc()).or_default() += 1;
                    if packet.payload_type() == payload_type {
                        audio_source = Some(packet.ssrc());
                        let peak = match format {
                            AudioFormat::Pcmu | AudioFormat::Pcma => packet
                                .payload()
                                .iter()
                                .map(|byte| i32::from(mulaw_decode(*byte)).abs())
                                .max(),
                            AudioFormat::Opus => OpusCodec::new()?
                                .decode(packet.payload())?
                                .samples()
                                .iter()
                                .map(|sample| i32::from(*sample).abs())
                                .max(),
                        }
                        .ok_or(MediaError::InvalidFrame)?;
                        assert!(peak > 1000);
                    } else {
                        assert_eq!(packet.payload_type(), 101);
                        event_source = Some(packet.ssrc());
                        let event = TelephoneEvent::parse(packet.payload())?;
                        assert_eq!(event.digit(), DtmfDigit::Hash);
                        if event.ended() {
                            assert_eq!(event.duration(), 480);
                            ends += 1;
                        }
                    }
                    if ends == 3 && audio_source.is_some() {
                        assert_eq!(audio_source == event_source, format == AudioFormat::Pcmu);
                        return Ok(());
                    }
                }
                Err(MediaError::InvalidPacket)
            })
            .await
            .map_err(failure)?
        }
        .await;
        stop.send_replace(true);
        let stats = task.await.map_err(failure)??;
        result?;
        assert_eq!(stats.packets_sent, u64::from(counts.values().sum::<u32>()));
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut bytes = [0; 1501];
            for _ in 0..8 {
                let (size, _) = peer
                    .control()
                    .recv_from(&mut bytes)
                    .await
                    .map_err(failure)?;
                let kinds = rusterisk_media::inspect_rtcp(&bytes[..size])?;
                if kinds.last() != Some(&ControlKind::Goodbye) {
                    continue;
                }
                assert_eq!(
                    kinds
                        .iter()
                        .filter(|kind| **kind == ControlKind::Sender)
                        .count(),
                    counts.len()
                );
                for report in bytes[..counts.len() * 28].as_chunks::<28>().0 {
                    let ssrc = u32::from_be_bytes(report[4..8].try_into().map_err(failure)?);
                    let packets = u32::from_be_bytes(report[20..24].try_into().map_err(failure)?);
                    assert_eq!(counts.get(&ssrc), Some(&packets));
                }
                let bye_sources = bytes[size - counts.len() * 4..size]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                    .collect::<Vec<_>>();
                assert_eq!(bye_sources.len(), counts.len());
                assert!(bye_sources.iter().all(|source| counts.contains_key(source)));
                return Ok(());
            }
            Err(MediaError::InvalidPacket)
        })
        .await
        .map_err(failure)??;
    }
    Ok(())
}
