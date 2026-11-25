use super::{cleanup::combine, failure, Active, DigitDelivery, MediaAdmission, Signaling};
use super::{Outcome, Waiting};
use crate::media::{MediaHub, MediaNotice, PlaybackControl, Queued};
use crate::rtp::SocketPair;
use crate::storage::{MusicSelection, StorageWorker};
use crate::{
    Action, CallKey, ControlError, Controller, Credentials, Effect, IncomingCall, IncomingTarget,
    MediaOwner,
};
use rusterisk_api::{
    ChannelId, ChannelState, EventKind, ExternalRtpTarget, MediaUri, RecordingOptions,
    StaticRtpCodec,
};
use rusterisk_config::Codec;
use rusterisk_config::Config;
use rusterisk_core::Wake;
use rusterisk_core::{AudioGate, PeerMediaDirection};
use rusterisk_core::{
    DtmfCommand, DtmfDigit, DtmfOutbox, DtmfTiming, MediaTaskKind, MediaTaskToken, MediaTasks,
    RecordingControl,
};
use rusterisk_media::{Frame, MediaError};
use rusterisk_sip::{Direction, Output, Session, SessionSecurity, SipEvent};
use rusterisk_storage::Recorder;
use std::net::Ipv4Addr;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::watch;

// A turn that waits for the media task holds an unrelated response for five seconds.
const UNRELATED: Duration = Duration::from_secs(1);

fn id(name: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(name.to_owned())?)
}

fn configuration() -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[phone]\ntype=aor\nmax_contacts=1\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=phone\ncontext=internal\nallow=ulaw\n",
        "",
        "[internal]\nexten => 100,1,Stasis(calls)\n",
    )
    .and_then(|config| {
        config
            .with_music_on_hold_configuration("[default]\nmode=files\ndirectory=music\nsort=alpha\n")
    })
    .map_err(failure)
}

// Stores one music track for the default class.
fn track(directory: &std::path::Path) -> Result<(), ControlError> {
    let mut recorder = Recorder::create(directory, "held-music", 8000).map_err(failure)?;
    recorder
        .write(Frame::new(&[1200; 160], 8000).map_err(failure)?)
        .map_err(failure)?;
    let wave = recorder.finish().map_err(failure)?;
    std::fs::create_dir(directory.join("music")).map_err(failure)?;
    std::fs::rename(wave, directory.join("music/known.wav")).map_err(failure)
}

// A media task that queues commands and answers none, beside a served control task.
struct Silent {
    controller: Controller,
    signaling: Signaling,
    media: Option<MediaHub>,
    answering: Option<tokio::task::JoinHandle<Result<(), MediaError>>>,
    music: Option<MusicSelection>,
    requests: tokio::task::JoinSet<Result<crate::Outcome, ControlError>>,
}

impl Silent {
    fn queued(&mut self) -> Option<Queued> {
        self.media.as_mut().and_then(MediaHub::take_queued)
    }

    // Starts the media task, which then answers the commands that wait in its queue.
    fn answer(&mut self) -> Result<(), ControlError> {
        let media = self.media.take().ok_or(ControlError::Capacity)?;
        self.answering = Some(tokio::spawn(media.run(self.controller.shutdown_signal())));
        Ok(())
    }

    // Requests an action and applies its effect within the time an unrelated response allows.
    // A recording request returns only after its admission, so the request stays pending.
    async fn apply(&mut self, action: Action) -> Result<Effect, ControlError> {
        let requester = self.controller.clone();
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        self.requests
            .spawn(async move { requester.execute(credentials, None, action).await });
        let effect = self.signaling.next_effect_for_test().await?;
        self.turn(effect.clone()).await?;
        Ok(effect)
    }

    // Requests an action and serves the call decisions until its answer arrives.
    async fn request(&mut self, action: Action) -> Result<crate::Outcome, ControlError> {
        let controller = self.controller.clone();
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        self.signaling
            .control_until(controller.execute(credentials, None, action))
            .await?
    }

    async fn turn(&mut self, effect: Effect) -> Result<(), ControlError> {
        tokio::time::timeout(UNRELATED, self.signaling.effect(effect, 0))
            .await
            .map_err(|_| ControlError::Invalid("The turn waited for the media task"))?
    }

    // Serves the signaling task until a peer receives a response to its REGISTER.
    async fn registers(&mut self) -> Result<(), ControlError> {
        let target = self
            .signaling
            .listeners()?
            .first()
            .map(|(_, address)| *address)
            .ok_or(ControlError::Capacity)?;
        let peer = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
        let local = peer.local_addr().map_err(failure)?;
        let request = format!("REGISTER sip:localhost SIP/2.0\r\nVia: SIP/2.0/UDP {local};branch=z9hG4bK-unrelated;rport\r\nFrom: <sip:phone@localhost>;tag=phone\r\nTo: <sip:phone@localhost>\r\nCall-ID: unrelated-registration\r\nCSeq: 1 REGISTER\r\nContact: <sip:phone@{local}>\r\nExpires: 120\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n");
        let (stop, mut stopped) = watch::channel(false);
        let clock = self.signaling.controller.clock();
        let mut serving = std::pin::pin!(self.signaling.serve(&mut stopped, clock));
        let answered = async {
            peer.send_to(request.as_bytes(), target)
                .await
                .map_err(failure)?;
            let mut bytes = [0; 2048];
            let (size, _) = tokio::time::timeout(UNRELATED, peer.recv_from(&mut bytes))
                .await
                .map_err(|_| ControlError::Invalid("The REGISTER waited for the media task"))?
                .map_err(failure)?;
            if bytes[..size].starts_with(b"SIP/2.0 ") {
                Ok(())
            } else {
                Err(ControlError::Invalid("Expected a SIP response"))
            }
        };
        let result = tokio::select! {
            result = answered => result,
            result = &mut serving => {
                return combine(Err(ControlError::Invalid("Signaling stopped before the response")), result);
            }
        };
        stop.send_replace(true);
        combine(result, serving.await)
    }
}

async fn silent(
    exercise: impl AsyncFnOnce(&mut Silent) -> Result<(), ControlError>,
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let started = match track(directory.path()) {
        Ok(()) => StorageWorker::start(directory.path().to_owned())
            .await
            .map_err(failure),
        Err(error) => Err(error),
    };
    let result = match started {
        Ok((storage, worker)) => {
            let result = async {
                let config = configuration()?;
                let music = storage
                    .prepare_music(config.music_on_hold.clone())
                    .await
                    .map_err(failure)?
                    .select("default");
                let (controller, runner, _effects) = Controller::new(config.clone(), 2);
                let (port, media, notices) = MediaHub::new(2);
                let bound = Signaling::bind(config, controller.clone(), runner, port, notices)
                    .await
                    .map(|signaling| signaling.with_storage(storage));
                let result = match bound {
                    Ok(signaling) => {
                        let mut fixture = Silent {
                            controller: controller.clone(),
                            signaling,
                            media: Some(media),
                            answering: None,
                            music,
                            requests: tokio::task::JoinSet::new(),
                        };
                        let result = exercise(&mut fixture).await;
                        fixture.requests.shutdown().await;
                        let result = combine(result, fixture.signaling.network.close().await);
                        controller.shutdown();
                        match fixture.answering.take() {
                            Some(answering) => combine(
                                result,
                                tokio::time::timeout(Duration::from_secs(3), answering)
                                    .await
                                    .map_err(failure)
                                    .and_then(|joined| joined.map_err(failure))
                                    .and_then(|served| served.map_err(failure)),
                            ),
                            None => result,
                        }
                    }
                    Err(error) => Err(error),
                };
                controller.shutdown();
                result
            }
            .await;
            combine(result, worker.close().await.map_err(failure))
        }
        Err(error) => Err(error),
    };
    combine(result, directory.close().map_err(failure))
}

fn bridge() -> Result<Action, ControlError> {
    Ok(Action::CreateBridge {
        id: id("held")?,
        name: "held".into(),
    })
}

fn playback() -> Result<Action, ControlError> {
    Ok(Action::PlayBridge {
        bridge: id("held")?,
        id: id("pending-playback")?,
        media: MediaUri::try_from("sound:known").map_err(failure)?,
        language: None,
        options: Default::default(),
    })
}

#[tokio::test]
async fn a_playback_reservation_leaves_the_turn_free() -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        fixture.request(bridge()?).await?;
        fixture.apply(playback()?).await?;
        fixture.registers().await
    })
    .await
}

#[tokio::test]
async fn a_recording_reservation_with_its_prompt_leaves_the_turn_free() -> Result<(), ControlError>
{
    silent(async |fixture: &mut Silent| {
        fixture.request(bridge()?).await?;
        fixture
            .apply(Action::RecordBridge {
                bridge: id("held")?,
                name: id("pending-recording")?,
                options: RecordingOptions {
                    beep: true,
                    ..Default::default()
                },
            })
            .await?;
        fixture.registers().await
    })
    .await
}

#[tokio::test]
async fn a_stop_after_a_reservation_stays_behind_it_in_the_queue() -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        fixture.request(bridge()?).await?;
        let Effect::PlayMedia { token, .. } = fixture.apply(playback()?).await? else {
            return Err(ControlError::Invalid("Expected the playback effect"));
        };
        fixture
            .turn(Effect::ControlPlayback {
                token,
                operation: PlaybackControl::Stop,
            })
            .await?;
        assert_eq!(fixture.queued(), Some(Queued::Reservation));
        assert_eq!(fixture.queued(), Some(Queued::Stop));
        assert_eq!(fixture.queued(), None);
        Ok(())
    })
    .await
}

// Records a call that signaling serves and the media task has never attached.
fn unattached(fixture: &mut Silent) -> Result<(CallKey, MediaTaskToken), ControlError> {
    let key = CallKey {
        id: id("unattached")?,
        generation: 7,
    };
    fixture.signaling.calls.insert(
        key.clone(),
        Active {
            listener: 0,
            media: MediaAdmission::Attached,
            digits: DigitDelivery::Media,
            direction_reply: None,
        },
    );
    let mut tasks = MediaTasks::new(key.generation);
    let token = tasks.reserve(MediaTaskKind::Playback)?;
    tasks.start(token)?;
    Ok((key, token))
}

#[tokio::test]
async fn a_music_reservation_leaves_the_turn_free() -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let (key, token) = unattached(fixture)?;
        let selection = fixture.music.clone().ok_or(ControlError::Capacity)?;
        fixture
            .turn(Effect::StartMusic {
                key,
                token,
                selection: Ok(selection),
            })
            .await?;
        assert_eq!(fixture.queued(), Some(Queued::Reservation));
        fixture.registers().await
    })
    .await
}

#[tokio::test]
async fn a_refused_reservation_fails_its_job_with_the_reason_of_the_media_task(
) -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let (key, token) = unattached(fixture)?;
        fixture
            .turn(Effect::Playback {
                key: key.clone(),
                token,
                media: "known".into(),
            })
            .await?;
        fixture.answer()?;
        let (owner, failed, result) =
            tokio::time::timeout(UNRELATED, fixture.signaling.playbacks.join_next())
                .await
                .map_err(failure)?
                .ok_or(ControlError::Capacity)?
                .map_err(failure)?;
        assert!(matches!(owner, MediaOwner::Channel(owned) if owned == key));
        assert_eq!(failed, token);
        assert!(matches!(
            result,
            Err(ControlError::Adapter(reason)) if reason == MediaError::UnknownStream.to_string()
        ));
        Ok(())
    })
    .await
}

fn digits() -> Result<DtmfCommand, ControlError> {
    let timing = DtmfTiming::new(3, 20, 60, 4).map_err(|_| ControlError::Invalid("test timing"))?;
    DtmfOutbox::new(u16::MAX)
        .and_then(|mut outbox| outbox.admit(&[DtmfDigit::Five], timing))
        .map_err(|_| ControlError::Invalid("test digits"))
}

#[tokio::test]
async fn a_digit_command_leaves_the_turn_free() -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let (key, _token) = unattached(fixture)?;
        fixture
            .turn(Effect::Dtmf {
                key,
                command: digits()?,
            })
            .await?;
        assert_eq!(fixture.queued(), Some(Queued::Other));
        fixture.registers().await
    })
    .await
}

#[tokio::test]
async fn a_recording_control_leaves_the_turn_free() -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let (_key, token) = unattached(fixture)?;
        fixture
            .turn(Effect::ControlRecording {
                token,
                operation: RecordingControl::Pause,
            })
            .await?;
        assert_eq!(fixture.queued(), Some(Queued::Other));
        fixture.registers().await
    })
    .await
}

#[tokio::test]
async fn a_refused_digit_command_reports_its_finish() -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let (key, _token) = unattached(fixture)?;
        let command = digits()?;
        fixture
            .turn(Effect::Dtmf {
                key: key.clone(),
                command,
            })
            .await?;
        fixture.answer()?;
        let notice = tokio::time::timeout(UNRELATED, fixture.signaling.notices.recv())
            .await
            .map_err(failure)?
            .ok_or(ControlError::Capacity)?;
        assert!(matches!(
            notice,
            MediaNotice::DtmfFinished { key: finished, token }
                if finished == key && token == command.token
        ));
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_full_command_queue_fails_one_digit_delivery_and_keeps_signaling_running(
) -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let controller = fixture.controller.clone();
        let _subscription = fixture
            .signaling
            .control_until(controller.subscribe(
                Credentials::new("app".into(), "test-password".into())?,
                None,
                "app".into(),
            ))
            .await??;
        let key = fixture
            .signaling
            .control_until(controller.incoming(IncomingCall {
                id: id("digits")?,
                endpoint: "phone".into(),
                caller: "100".into(),
                context: "internal".into(),
                extension: "100".into(),
                target: IncomingTarget::Stasis {
                    application: "app".into(),
                    arguments: Vec::new(),
                },
            }))
            .await??;
        fixture.signaling.calls.insert(
            key.clone(),
            Active {
                listener: 0,
                media: MediaAdmission::Attached,
                digits: DigitDelivery::Media,
                direction_reply: None,
            },
        );
        let (_unattached, token) = unattached(fixture)?;
        while fixture.signaling.media.stop_playback(token).is_ok() {}
        let received = SipEvent::DtmfReceived {
            id: key.id.as_str().into(),
            generation: key.generation,
            digit: DtmfDigit::Five,
            duration_ms: 40,
        };
        tokio::time::timeout(
            UNRELATED,
            fixture.signaling.output_for_test(
                0,
                Output {
                    datagrams: Vec::new(),
                    events: Vec::from([received]),
                },
                0,
            ),
        )
        .await
        .map_err(|_| ControlError::Invalid("The turn waited for queue space"))??;
        fixture.registers().await
    })
    .await
}

// Originates a call through control and records it with sockets that the media task has not attached.
async fn dialed(
    fixture: &mut Silent,
) -> Result<(CallKey, crate::Subscription, SipEvent), ControlError> {
    let credentials = Credentials::new("app".into(), "test-password".into())?;
    let controller = fixture.controller.clone();
    let subscription = fixture
        .signaling
        .control_until(controller.subscribe(credentials.clone(), None, "app".into()))
        .await??;
    fixture
        .signaling
        .control_until(controller.execute(
            credentials,
            None,
            Action::Originate {
                id: id("dialed")?,
                endpoint: "PJSIP/phone".into(),
                app: "app".into(),
                args: Vec::new(),
                caller: "100".into(),
                timeout: 30,
            },
        ))
        .await??;
    let Effect::Start { key, .. } = fixture.signaling.next_effect_for_test().await? else {
        return Err(ControlError::Invalid("Expected the start effect"));
    };
    let sockets = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    fixture.signaling.calls.insert(
        key.clone(),
        Active {
            listener: 0,
            media: MediaAdmission::Rtp(sockets),
            digits: DigitDelivery::Media,
            direction_reply: None,
        },
    );
    let answered = SipEvent::Answered {
        id: key.id.as_str().into(),
        generation: key.generation,
        session: Session {
            remote: peer.local_addr().map_err(failure)?,
            codec: Codec::Pcmu,
            payload_type: 0,
            telephone: None,
            direction: Direction::SendRecv,
            security: SessionSecurity::Rtp,
        },
    };
    Ok((key, subscription, answered))
}

impl Silent {
    async fn receive(&mut self, event: SipEvent, now: u64) -> Result<(), ControlError> {
        tokio::time::timeout(
            UNRELATED,
            self.signaling.output_for_test(
                0,
                Output {
                    datagrams: Vec::new(),
                    events: Vec::from([event]),
                },
                now,
            ),
        )
        .await
        .map_err(|_| ControlError::Invalid("The turn waited for the media task"))?
    }
}

#[tokio::test]
async fn a_recording_stop_waits_with_its_channel_reservation() -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let (key, _subscription, answered) = dialed(fixture).await?;
        fixture
            .signaling
            .notify(crate::Signal::Milestone {
                key: key.clone(),
                milestone: crate::Milestone::Answered,
            })
            .await?;
        fixture.receive(answered, 0).await?;
        fixture
            .apply(Action::RecordChannel {
                channel: key.id.clone(),
                name: id("held-stop")?,
                options: RecordingOptions::default(),
            })
            .await?;
        fixture
            .apply(Action::EndRecording {
                name: id("held-stop")?,
                mode: crate::media::RecordingEnd::Cancel,
            })
            .await?;
        assert_eq!(fixture.queued(), Some(Queued::Other));
        assert_eq!(fixture.queued(), None);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn an_expired_attachment_releases_through_a_full_command_queue() -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let (key, _subscription, answered) = dialed(fixture).await?;
        fixture.receive(answered, 0).await?;
        while fixture.signaling.media.bridge(71, Vec::new()).is_ok() {}
        fixture.signaling.expire_pending(5000).await?;
        let mut releases = 0;
        while let Some(command) = fixture.queued() {
            if command == Queued::Release {
                releases += 1;
            }
        }
        assert_eq!(releases, 1);
        assert_eq!(fixture.signaling.waiting_serial(&key), None);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn an_answered_attachment_leaves_the_turn_free() -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let (_key, _subscription, answered) = dialed(fixture).await?;
        fixture.receive(answered, 0).await?;
        assert_eq!(fixture.queued(), Some(Queued::Other));
        fixture.registers().await
    })
    .await
}

fn label(kind: &EventKind) -> Option<&'static str> {
    if matches!(kind, EventKind::ChannelStateChange { channel } if channel.state == ChannelState::Up)
    {
        Some("answered")
    } else if matches!(kind, EventKind::ChannelDtmfReceived { .. }) {
        Some("digit")
    } else if matches!(kind, EventKind::ChannelDestroyed { .. }) {
        Some("destroyed")
    } else {
        None
    }
}

// The labelled events of one call up to its end, in the order that control published them.
async fn published(
    subscription: &mut crate::Subscription,
) -> Result<Vec<&'static str>, ControlError> {
    let mut labels = Vec::new();
    tokio::time::timeout(UNRELATED, async {
        while labels.last() != Some(&"destroyed") {
            let event = subscription
                .events
                .recv()
                .await
                .ok_or(ControlError::Capacity)?;
            labels.extend(label(&event.kind));
        }
        Ok::<(), ControlError>(())
    })
    .await
    .map_err(failure)??;
    Ok(labels)
}

#[tokio::test]
async fn an_unanswered_attachment_ends_its_call_with_cause_21_at_its_deadline(
) -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let (_key, mut subscription, answered) = dialed(fixture).await?;
        fixture.receive(answered, 100).await?;
        let clock = fixture.signaling.controller.clock();
        assert_eq!(fixture.signaling.wake(100), Wake::At(5100));
        fixture.signaling.turn(5099, clock).await?;
        assert_eq!(fixture.signaling.pending_wake(), Wake::At(5100));
        fixture.signaling.turn(5100, clock).await?;
        assert_eq!(fixture.signaling.pending_wake(), Wake::Idle);
        assert_eq!(fixture.queued(), Some(Queued::Other));
        assert_eq!(fixture.queued(), Some(Queued::Release));
        assert_eq!(destroyed(&mut subscription).await?, 21);
        Ok(())
    })
    .await
}

// The cause with which control ended the call.
async fn destroyed(subscription: &mut crate::Subscription) -> Result<u16, ControlError> {
    tokio::time::timeout(UNRELATED, async {
        loop {
            let event = subscription
                .events
                .recv()
                .await
                .ok_or(ControlError::Capacity)?;
            if let EventKind::ChannelDestroyed { cause, .. } = event.kind {
                return Ok::<u16, ControlError>(cause);
            }
        }
    })
    .await
    .map_err(failure)?
}

#[tokio::test]
async fn a_direction_change_that_cannot_start_ends_its_call_and_not_the_signaling_task(
) -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let (key, mut subscription, _answered) = dialed(fixture).await?;
        let (_other, token) = unattached(fixture)?;
        while fixture.signaling.media.stop_playback(token).is_ok() {}
        fixture
            .turn(Effect::PeerDirection {
                key,
                direction: PeerMediaDirection::SendOnly,
                gate: AudioGate::Open,
                generated_audio_gate: AudioGate::Open,
            })
            .await?;
        assert_eq!(destroyed(&mut subscription).await?, 21);
        Ok(())
    })
    .await
}

impl Silent {
    // Answers the attachment that the call waits for.
    async fn attached(&mut self, key: &CallKey, now: u64) -> Result<(), ControlError> {
        let serial = self
            .signaling
            .waiting_serial(key)
            .ok_or(ControlError::Invalid("The call does not wait"))?;
        self.signaling
            .completed(key.clone(), Outcome::Attached(serial, Ok(())), now)
            .await
    }
}

#[tokio::test]
async fn an_answer_to_an_expired_wait_completes_no_later_wait() -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let key = CallKey {
            id: id("external")?,
            generation: 9,
        };
        let local = "127.0.0.1:4000".parse().map_err(failure)?;
        let release = fixture
            .signaling
            .media
            .reserve_attachment_release(1)
            .map_err(failure)?;
        fixture
            .signaling
            .pend(key.clone(), 1, Waiting::External { local, release }, 0);
        fixture.signaling.expire_pending(5000).await?;
        assert_eq!(fixture.queued(), Some(Queued::Release));
        let release = fixture
            .signaling
            .media
            .reserve_attachment_release(2)
            .map_err(failure)?;
        fixture
            .signaling
            .pend(key.clone(), 2, Waiting::External { local, release }, 5001);
        fixture
            .signaling
            .completed(key.clone(), Outcome::Attached(1, Ok(())), 5002)
            .await?;
        assert_eq!(fixture.signaling.waiting_serial(&key), Some(2));
        assert!(!fixture.signaling.external_media.contains(&key));
        fixture
            .signaling
            .completed(key.clone(), Outcome::Attached(2, Ok(())), 5003)
            .await?;
        assert_eq!(fixture.signaling.waiting_serial(&key), None);
        assert!(fixture.signaling.external_media.contains(&key));
        assert_eq!(fixture.queued(), None);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_held_direction_change_keeps_the_release_behind_its_own_wait() -> Result<(), ControlError>
{
    silent(async |fixture: &mut Silent| {
        let (key, _subscription, answered) = dialed(fixture).await?;
        fixture.receive(answered, 0).await?;
        fixture
            .turn(Effect::PeerDirection {
                key: key.clone(),
                direction: PeerMediaDirection::SendOnly,
                gate: AudioGate::Open,
                generated_audio_gate: AudioGate::Open,
            })
            .await?;
        fixture.turn(Effect::Release { key: key.clone() }).await?;
        let attachment = fixture.signaling.waiting_serial(&key);
        fixture.attached(&key, 1).await?;
        let direction = fixture
            .signaling
            .waiting_serial(&key)
            .ok_or(ControlError::Invalid("The direction change did not wait"))?;
        assert_ne!(Some(direction), attachment);
        assert!(fixture.signaling.calls.contains_key(&key));
        fixture
            .signaling
            .completed(key.clone(), Outcome::Direction(direction, Ok(true)), 2)
            .await?;
        assert!(!fixture.signaling.calls.contains_key(&key));
        assert_eq!(fixture.signaling.pending_wake(), Wake::Idle);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_call_that_holds_too_much_ends_once_and_is_released_after_its_wait(
) -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let (key, mut subscription, answered) = dialed(fixture).await?;
        fixture.receive(answered, 0).await?;
        for now in 1..=18 {
            fixture
                .receive(
                    SipEvent::DtmfReceived {
                        id: key.id.as_str().into(),
                        generation: key.generation,
                        digit: DtmfDigit::Five,
                        duration_ms: 40,
                    },
                    now,
                )
                .await?;
        }
        assert_eq!(destroyed(&mut subscription).await?, 42);
        assert!(fixture.signaling.calls.contains_key(&key));
        fixture.attached(&key, 19).await?;
        assert!(!fixture.signaling.calls.contains_key(&key));
        Ok(())
    })
    .await
}

#[tokio::test]
async fn events_for_a_waiting_call_follow_its_answer_in_arrival_order() -> Result<(), ControlError>
{
    silent(async |fixture: &mut Silent| {
        let (key, mut subscription, answered) = dialed(fixture).await?;
        fixture.receive(answered, 0).await?;
        fixture
            .receive(
                SipEvent::DtmfReceived {
                    id: key.id.as_str().into(),
                    generation: key.generation,
                    digit: DtmfDigit::Five,
                    duration_ms: 40,
                },
                1,
            )
            .await?;
        fixture
            .receive(
                SipEvent::Ended {
                    id: key.id.as_str().into(),
                    generation: key.generation,
                    cause: 16,
                },
                2,
            )
            .await?;
        fixture.attached(&key, 3).await?;
        assert_eq!(
            published(&mut subscription).await?,
            ["answered", "digit", "destroyed"]
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_direction_change_leaves_the_turn_free() -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let (key, _token) = unattached(fixture)?;
        fixture
            .turn(Effect::PeerDirection {
                key,
                direction: PeerMediaDirection::SendOnly,
                gate: AudioGate::Open,
                generated_audio_gate: AudioGate::Open,
            })
            .await?;
        assert_eq!(fixture.queued(), Some(Queued::Other));
        fixture.registers().await
    })
    .await
}

#[tokio::test]
async fn an_external_connection_leaves_the_turn_free() -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
        let target =
            ExternalRtpTarget::new(peer.local_addr().map_err(failure)?, StaticRtpCodec::Pcmu)
                .map_err(failure)?;
        fixture
            .turn(Effect::ConnectExternalRtp {
                key: CallKey {
                    id: id("external")?,
                    generation: 9,
                },
                target,
            })
            .await?;
        assert_eq!(fixture.queued(), Some(Queued::Other));
        fixture.registers().await
    })
    .await
}

#[tokio::test]
async fn a_release_for_a_waiting_call_follows_its_answer() -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let (key, mut subscription, answered) = dialed(fixture).await?;
        fixture.receive(answered, 0).await?;
        fixture.turn(Effect::Release { key: key.clone() }).await?;
        assert!(fixture.signaling.calls.contains_key(&key));
        fixture.attached(&key, 1).await?;
        assert!(!fixture.signaling.calls.contains_key(&key));
        assert_eq!(fixture.signaling.pending_wake(), Wake::Idle);
        let answered = tokio::time::timeout(UNRELATED, async {
            loop {
                let event = subscription
                    .events
                    .recv()
                    .await
                    .ok_or(ControlError::Capacity)?;
                if label(&event.kind) == Some("answered") {
                    return Ok::<(), ControlError>(());
                }
            }
        })
        .await
        .map_err(failure)?;
        answered
    })
    .await
}

impl Silent {
    // Serves the composed task until a control request finishes within the time an unrelated response allows.
    async fn answers(&mut self, action: Action) -> Result<crate::Outcome, ControlError> {
        let controller = self.controller.clone();
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        let (stop, mut stopped) = watch::channel(false);
        let clock = self.signaling.controller.clock();
        let mut serving = std::pin::pin!(self.signaling.serve(&mut stopped, clock));
        let answered =
            tokio::time::timeout(UNRELATED, controller.execute(credentials, None, action));
        let result = tokio::select! {
            result = answered => result.map_err(|_| ControlError::Invalid("The request waited for the media task"))?,
            result = &mut serving => {
                return combine(Err(ControlError::Invalid("Signaling stopped before the answer")), result).map(|()| crate::Outcome::empty());
            }
        };
        stop.send_replace(true);
        let served = serving.await;
        match (result, served) {
            (Ok(outcome), Ok(())) => Ok(outcome),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }
}

#[tokio::test]
async fn a_control_request_finishes_while_an_attachment_waits_for_a_silent_media_task(
) -> Result<(), ControlError> {
    silent(async |fixture: &mut Silent| {
        let (key, _subscription, answered) = dialed(fixture).await?;
        fixture.receive(answered, 0).await?;
        assert!(fixture.signaling.waits(&key));
        let created = fixture.answers(bridge()?).await?;
        assert!((200..300).contains(&created.status));
        assert!(fixture.signaling.waits(&key));
        Ok(())
    })
    .await
}
