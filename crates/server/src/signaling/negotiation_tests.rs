use super::{Active, DigitDelivery, Signaling};
use crate::media::MediaHub;
use crate::rtp::{RtpSession, SocketPair};
use crate::{CallKey, ControlError, Controller, Effect};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_core::{AudioGate, PeerMediaDirection};
use rusterisk_media::AudioFormat;
use rusterisk_sip::{Datagram, Direction, Output, SipEvent};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use tokio::net::UdpSocket;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

#[tokio::test]
async fn direction_answers_wait_for_the_current_transport_and_ignore_cached_retries(
) -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 1);
    let (media, hub, notices) = MediaHub::new(1);
    let mut signaling =
        Signaling::bind(config, controller.clone(), runner, media.clone(), notices).await?;
    let peer = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let address = peer.local_addr().map_err(failure)?;
    let audio = SocketPair::bind(IpAddr::V4(Ipv4Addr::LOCALHOST), 0, 0).map_err(failure)?;
    let key = CallKey {
        id: ChannelId::try_from("call".to_owned())?,
        generation: 1,
    };
    signaling.calls.insert(
        key.clone(),
        Active {
            listener: 0,
            media: super::MediaAdmission::Attached,
            digits: DigitDelivery::Media,
            direction_reply: None,
        },
    );
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let result = async {
        media
            .attach(
                key.clone(),
                SocketPair::bind(IpAddr::V4(Ipv4Addr::LOCALHOST), 0, 0).map_err(failure)?,
                RtpSession {
                    remote: audio.local_addr().map_err(failure)?,
                    format: AudioFormat::Pcmu,
                    payload_type: 0,
                    telephone: None,
                },
                PeerMediaDirection::SendReceive,
            )
            .await
            .map_err(failure)?;
        let bytes = b"negotiated answer".to_vec();
        signaling
            .output(
                0,
                Output {
                    datagrams: [Datagram::new(address, bytes.clone())]
                        .into_iter()
                        .collect(),
                    events: [SipEvent::PeerDirection {
                        id: key.id.to_string(),
                        generation: 1,
                        direction: Direction::SendOnly,
                    }]
                    .into_iter()
                    .collect(),
                },
                0,
            )
            .await?;
        signaling
            .output(
                0,
                Output {
                    datagrams: [Datagram::new(address, bytes.clone())]
                        .into_iter()
                        .collect(),
                    events: Vec::new(),
                },
                1,
            )
            .await?;
        let mut received = [0; 128];
        assert!(
            tokio::time::timeout(Duration::from_millis(100), peer.recv_from(&mut received))
                .await
                .is_err()
        );
        signaling
            .effect(
                Effect::PeerDirection {
                    key: CallKey {
                        generation: 2,
                        ..key.clone()
                    },
                    direction: PeerMediaDirection::SendOnly,
                    gate: AudioGate::OutgoingMuted,
                    generated_audio_gate: AudioGate::OutgoingMuted,
                },
                2,
            )
            .await?;
        signaling.media_answers_for_test(2).await?;
        assert!(peer.try_recv_from(&mut received).is_err());
        signaling
            .effect(
                Effect::PeerDirection {
                    key,
                    direction: PeerMediaDirection::SendOnly,
                    gate: AudioGate::OutgoingMuted,
                    generated_audio_gate: AudioGate::OutgoingMuted,
                },
                3,
            )
            .await?;
        signaling.media_answers_for_test(3).await?;
        let (size, _) = tokio::time::timeout(Duration::from_secs(2), peer.recv_from(&mut received))
            .await
            .map_err(failure)?
            .map_err(failure)?;
        assert_eq!(&received[..size], bytes);
        assert!(peer.try_recv_from(&mut received).is_err());
        Ok(())
    }
    .await;
    controller.shutdown();
    hub.await.map_err(failure)?.map_err(failure)?;
    result
}
