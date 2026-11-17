use crate::media::MediaHub;
use crate::rtp::{RtpSession, SocketPair};
use crate::signaling::{Active, DigitDelivery, Signaling};
use crate::{CallKey, ControlError, Controller, Effect};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_core::{AudioGate, PeerMediaDirection};
use rusterisk_media::AudioFormat;
use rusterisk_sip::{Datagram, Direction, Output, SipEvent};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::sync::mpsc::error::TryRecvError;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn answer(key: &CallKey, peer: SocketAddr, bytes: &[u8]) -> Output {
    Output {
        datagrams: [Datagram::new(peer, bytes.to_vec())].into_iter().collect(),
        events: [SipEvent::PeerDirection {
            id: key.id.to_string(),
            generation: key.generation,
            direction: Direction::SendOnly,
        }]
        .into_iter()
        .collect(),
    }
}

#[tokio::test]
async fn media_acknowledgements_never_send_old_answers_on_replacement_connections(
) -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        "[stream]\ntype=transport\nprotocol=tcp\nbind=127.0.0.1:0\n",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, _runner, effects) = Controller::new(config.clone(), 8);
    let (media, hub, notices) = MediaHub::new(8);
    let mut signaling =
        Signaling::bind(config, controller.clone(), effects, media.clone(), notices).await?;
    let key = CallKey {
        id: ChannelId::try_from("deferred-call".to_owned())?,
        generation: 1,
    };
    signaling.calls.insert(
        key.clone(),
        Active {
            listener: 0,
            pending: None,
            digits: DigitDelivery::Media,
            direction_reply: None,
        },
    );
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let result = async {
        let remote = SocketPair::bind(IpAddr::V4(Ipv4Addr::LOCALHOST), 0, 0).map_err(failure)?;
        media
            .attach(
                key.clone(),
                SocketPair::bind(IpAddr::V4(Ipv4Addr::LOCALHOST), 0, 0).map_err(failure)?,
                RtpSession {
                    remote: remote.local_addr().map_err(failure)?,
                    format: AudioFormat::Pcmu,
                    payload_type: 0,
                    telephone: None,
                },
                PeerMediaDirection::SendReceive,
            )
            .await
            .map_err(failure)?;
        let peer = "127.0.0.1:5061".parse().map_err(failure)?;
        let previous = signaling.network.reserve(0, peer)?;
        signaling
            .output(0, answer(&key, peer, b"old answer"), 0)
            .await?;
        signaling.network.ports.remove(&(0, peer));
        let mut current = signaling.network.reserve(0, peer)?;
        assert!(current.key.generation > previous.key.generation);
        signaling
            .effect(
                Effect::PeerDirection {
                    key: key.clone(),
                    direction: PeerMediaDirection::SendOnly,
                    gate: AudioGate::OutgoingMuted,
                    generated_audio_gate: AudioGate::OutgoingMuted,
                },
                1,
            )
            .await?;
        assert!(matches!(
            current.packets.try_recv(),
            Err(TryRecvError::Empty)
        ));
        signaling
            .output(0, answer(&key, peer, b"current answer"), 2)
            .await?;
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
        assert_eq!(
            current.packets.try_recv().map_err(failure)?,
            b"current answer"
        );
        assert!(matches!(
            current.packets.try_recv(),
            Err(TryRecvError::Empty)
        ));
        signaling.network.close().await
    }
    .await;
    controller.shutdown();
    hub.await.map_err(failure)?.map_err(failure)?;
    result
}

#[tokio::test]
async fn closed_deferred_answers_suppress_cached_retries_on_replacement_connections(
) -> Result<(), ControlError> {
    for closing in [false, true] {
        let config = Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=test\n",
            "[stream]\ntype=transport\nprotocol=tcp\nbind=127.0.0.1:0\n",
            "",
            "",
        )
        .map_err(failure)?;
        let (controller, _runner, effects) = Controller::new(config.clone(), 8);
        let (media, _hub, notices) = MediaHub::new(8);
        let mut signaling = Signaling::bind(config, controller, effects, media, notices).await?;
        let key = CallKey {
            id: ChannelId::try_from("closed-deferred-call".to_owned())?,
            generation: 1,
        };
        signaling.calls.insert(
            key.clone(),
            Active {
                listener: 0,
                pending: None,
                digits: DigitDelivery::Media,
                direction_reply: None,
            },
        );
        let peer = "127.0.0.1:5061".parse().map_err(failure)?;
        let previous = if closing {
            let reserved = signaling.network.reserve(0, peer)?;
            signaling
                .network
                .ports
                .get_mut(&(0, peer))
                .ok_or(ControlError::Capacity)?
                .state = super::super::ConnectionState::Closing(ControlError::Capacity);
            Some(reserved)
        } else {
            None
        };
        signaling
            .output(0, answer(&key, peer, b"closed answer"), 0)
            .await?;
        signaling.network.ports.remove(&(0, peer));
        drop(previous);
        let mut replacement = signaling.network.reserve(0, peer)?;
        let mut retry = answer(&key, peer, b"closed answer");
        retry.events.clear();
        signaling.output(0, retry, 1).await?;
        assert!(matches!(
            replacement.packets.try_recv(),
            Err(TryRecvError::Empty)
        ));
        signaling.network.close().await?;
    }
    Ok(())
}
