use rusterisk_api::{Channel, ChannelId, ChannelState, EventKind};
use rusterisk_core::{DtmfDigit, DtmfTiming, DtmfTone};
use rusterisk_media::MediaError;
#[path = "webrtc_leg/driver.rs"]
mod webrtc_driver;

use rusterisk_media::MAX_PACKET;
use rusterisk_server::{
    media::MediaHub,
    media_link::{Channels, DigitCommand},
    signaling::Signaling,
    webrtc::{WebRtcLeg, WebRtcSession, WebRtcStats},
    Action, Controller, Credentials,
};
use std::time::Duration;
use tokio::net::UdpSocket;
#[path = "webrtc_digits/peer.rs"]
mod peer;
use peer::{failure, Clock};

struct Observed {
    states: Vec<ChannelState>,
    before_audio: Vec<serde_json::Value>,
    after_audio: Vec<serde_json::Value>,
    received: Vec<(ChannelId, String, u32)>,
    sent: Vec<DtmfTone>,
    duplicates: [Option<DtmfTone>; 2],
    stats: Vec<WebRtcStats>,
}

async fn send(
    controller: &Controller,
    credentials: &Credentials,
    channel: &ChannelId,
    digits: &str,
) -> Result<(), MediaError> {
    let accepted = controller
        .execute(
            credentials.clone(),
            None,
            Action::SendDtmf {
                channel: channel.clone(),
                digits: digits.into(),
                timing: DtmfTiming::new(0, 0, 1, 0)
                    .map_err(|error| failure(format!("{error:?}")))?,
            },
        )
        .await
        .map_err(failure)?;
    match accepted.body {
        None => Ok(()),
        Some(body) => Err(failure(format!("Unexpected DTMF control response: {body}"))),
    }
}

async fn connected(
    controller: &Controller,
    credentials: &Credentials,
    ids: &[ChannelId],
) -> Result<Vec<serde_json::Value>, MediaError> {
    let mut values = Vec::with_capacity(2);
    for id in ids {
        values.push(
            controller
                .execute(
                    credentials.clone(),
                    None,
                    Action::GetVariable {
                        channel: id.clone(),
                        variable: "RUSTERISK_MEDIA_CONNECTED".into(),
                    },
                )
                .await
                .map_err(failure)?
                .body
                .ok_or(MediaError::InvalidRoute)?,
        );
    }
    Ok(values)
}

async fn exchange(clock: Clock) -> Result<Observed, MediaError> {
    let peers = [
        UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?,
        UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?,
    ];
    let config = peer::configuration(&peers, clock)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices)
        .await
        .map_err(failure)?;
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let mut applications = Vec::with_capacity(2);
    let mut jobs = Vec::with_capacity(2);
    let mut phase = "SIP admission";
    let result = tokio::time::timeout(Duration::from_secs(12), async {
        let credentials =
            Credentials::new("app".into(), "test-password".into()).map_err(failure)?;
        let mut events = controller
            .subscribe(credentials.clone(), None, "calls".into())
            .await
            .map_err(failure)?;
        let mut ids = Vec::with_capacity(2);
        let mut states = Vec::with_capacity(4);
        for (name, peer) in ["left", "right"].into_iter().zip(&peers) {
            let id = ChannelId::try_from(name.to_owned()).map_err(failure)?;
            let response = controller
                .execute(
                    credentials.clone(),
                    None,
                    Action::Originate {
                        id: id.clone(),
                        endpoint: format!("PJSIP/{name}"),
                        app: "calls".into(),
                        args: Vec::new(),
                        caller: "100".into(),
                        timeout: 3,
                    },
                )
                .await
                .map_err(failure)?;
            let channel: Channel =
                serde_json::from_value(response.body.ok_or(MediaError::InvalidRoute)?)
                    .map_err(failure)?;
            states.push(channel.state);
            let session = peer::answer(peer, clock).await?;
            let (channels, application) = peer::channels();
            jobs.push(tokio::spawn(webrtc_driver::run(session, channels)));
            applications.push(application);
            loop {
                if let EventKind::StasisStart { channel, .. } = events
                    .events
                    .recv()
                    .await
                    .ok_or(MediaError::UnknownStream)?
                    .kind
                {
                    if channel.id == id {
                        states.push(channel.state);
                        break;
                    }
                }
            }
            ids.push(id);
        }
        let bridge = ChannelId::try_from("digits-bridge".to_owned()).map_err(failure)?;
        controller
            .execute(
                credentials.clone(),
                None,
                Action::CreateBridge {
                    id: bridge.clone(),
                    name: "WebRTC digits".into(),
                },
            )
            .await
            .map_err(failure)?;
        controller
            .execute(
                credentials.clone(),
                None,
                Action::AddChannels {
                    bridge: bridge.clone(),
                    channels: ids.clone(),
                },
            )
            .await
            .map_err(failure)?;
        let [left, right] = applications.as_mut_slice() else {
            return Err(MediaError::InvalidRoute);
        };
        phase = "received RFC4733 event before audio";
        left.commands
            .try_send(DigitCommand::Tone(DtmfTone {
                digit: DtmfDigit::Hash,
                duration_ms: 80,
            }))
            .map_err(failure)?;
        let mut received = Vec::with_capacity(16);
        loop {
            if let EventKind::ChannelDtmfReceived {
                digit,
                duration_ms,
                channel,
            } = events
                .events
                .recv()
                .await
                .ok_or(MediaError::UnknownStream)?
                .kind
            {
                received.push((channel.id, digit, duration_ms));
                break;
            }
        }
        let before_audio = connected(&controller, &credentials, &ids).await?;
        phase = "known duplex audio before digit controls";
        peer::audio(left, right).await?;
        let after_audio = connected(&controller, &credentials, &ids).await?;
        phase = "Controller digit sequence delivery";
        send(&controller, &credentials, &ids[0], "#1D").await?;
        let mut sent = Vec::with_capacity(16);
        for _ in 0..3 {
            sent.push(left.digits.recv().await.ok_or(MediaError::UnknownStream)?);
        }
        phase = "digit completion admission reuse";
        for digit in ["0", "1", "2", "3", "4", "5", "6", "7", "8"] {
            send(&controller, &credentials, &ids[0], digit).await?;
            sent.push(left.digits.recv().await.ok_or(MediaError::UnknownStream)?);
        }
        phase = "known duplex audio after digit controls";
        peer::audio(left, right).await?;
        let left_duplicate =
            match tokio::time::timeout(Duration::from_millis(150), left.digits.recv()).await {
                Err(_) => None,
                Ok(Some(tone)) => Some(tone),
                Ok(None) => return Err(MediaError::UnknownStream),
            };
        let right_duplicate = match right.digits.try_recv() {
            Ok(tone) => Some(tone),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => None,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                return Err(MediaError::UnknownStream);
            }
        };
        while let Ok(event) = events.events.try_recv() {
            if let EventKind::ChannelDtmfReceived {
                digit,
                duration_ms,
                channel,
            } = event.kind
            {
                if received.len() >= 16 {
                    return Err(MediaError::Capacity);
                }
                received.push((channel.id, digit, duration_ms));
            }
        }
        for id in ids {
            controller
                .execute(credentials.clone(), None, Action::Hangup(id))
                .await
                .map_err(failure)?;
        }
        controller
            .execute(credentials, None, Action::DestroyBridge(bridge))
            .await
            .map_err(failure)?;
        Ok::<_, MediaError>(Observed {
            states,
            before_audio,
            after_audio,
            received,
            sent,
            duplicates: [left_duplicate, right_duplicate],
            stats: Vec::with_capacity(2),
        })
    })
    .await
    .map_err(|error| failure(format!("{phase}: {error}")))
    .and_then(|result| result.map_err(|error| failure(format!("{phase}: {error}"))));
    for application in &applications {
        application.stop.send_replace(true);
    }
    let mut cleanup = Ok(());
    let mut stats = Vec::with_capacity(2);
    for job in jobs {
        match peer::finish(job).await {
            Ok(value) => stats.push(value),
            Err(error) => cleanup = peer::combine(cleanup, Err(error)),
        }
    }
    controller.shutdown();
    cleanup = peer::combine(cleanup, peer::finish(signaling).await);
    cleanup = peer::combine(cleanup, peer::finish(hub).await);
    match (result, cleanup) {
        (Ok(mut observed), Ok(())) => {
            observed.stats = stats;
            Ok(observed)
        }
        (Err(primary), Err(cleanup)) => {
            Err(failure(format!("{primary}; cleanup failed: {cleanup}")))
        }
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
    }
}

async fn verify(clock: Clock) -> Result<(), MediaError> {
    let observed = exchange(clock).await?;
    assert_eq!(
        observed.states,
        [
            ChannelState::Down,
            ChannelState::Up,
            ChannelState::Down,
            ChannelState::Up
        ]
    );
    assert_eq!(
        observed.before_audio,
        [
            serde_json::json!({"value":"0"}),
            serde_json::json!({"value":"0"})
        ]
    );
    assert_eq!(
        observed.after_audio,
        [
            serde_json::json!({"value":"1"}),
            serde_json::json!({"value":"1"})
        ]
    );
    assert_eq!(
        observed.received,
        [(
            ChannelId::try_from("left".to_owned()).map_err(failure)?,
            "#".to_owned(),
            80
        )]
    );
    let expected: Vec<_> = [
        DtmfDigit::Hash,
        DtmfDigit::One,
        DtmfDigit::D,
        DtmfDigit::Zero,
        DtmfDigit::One,
        DtmfDigit::Two,
        DtmfDigit::Three,
        DtmfDigit::Four,
        DtmfDigit::Five,
        DtmfDigit::Six,
        DtmfDigit::Seven,
        DtmfDigit::Eight,
    ]
    .into_iter()
    .map(|digit| DtmfTone {
        digit,
        duration_ms: 1,
    })
    .collect();
    assert_eq!(observed.sent, expected);
    assert_eq!(observed.duplicates, [None, None]);
    assert_eq!(observed.stats.len(), 2);
    for stats in observed.stats {
        assert!(stats.transport_connected);
        assert!(stats.audio.packets_received > 0);
        assert!(stats.audio.packets_sent > 0);
    }
    Ok(())
}

#[tokio::test]
async fn controller_digits_and_received_events_cross_the_owned_webrtc_media_hub(
) -> Result<(), MediaError> {
    verify(Clock::Matching).await
}

#[tokio::test]
async fn controller_digit_sequences_keep_opus_audio_and_an_independent_event_clock(
) -> Result<(), MediaError> {
    verify(Clock::Independent).await
}
