use super::{cleanup::combine, failure, Active, DigitDelivery, MediaAdmission, Signaling};
use crate::media::{MediaHub, MediaNotice, PlaybackControl, Queued};
use crate::storage::{MusicSelection, StorageWorker};
use crate::{
    Action, CallKey, ControlError, Controller, Credentials, Effect, IncomingCall, IncomingTarget,
    MediaOwner,
};
use rusterisk_api::{ChannelId, MediaUri, RecordingOptions};
use rusterisk_config::Config;
use rusterisk_core::{
    DtmfCommand, DtmfDigit, DtmfOutbox, DtmfTiming, MediaTaskKind, MediaTaskToken, MediaTasks,
    RecordingControl,
};
use rusterisk_media::{Frame, MediaError};
use rusterisk_sip::{Output, SipEvent};
use rusterisk_storage::Recorder;
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
        let effect = self
            .signaling
            .effects
            .recv()
            .await
            .ok_or(ControlError::Capacity)?;
        self.turn(effect.clone()).await?;
        Ok(effect)
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
                let (controller, runner, effects) = Controller::new(config.clone(), 2);
                let runner = tokio::spawn(runner.run());
                let (port, media, notices) = MediaHub::new(2);
                let bound = Signaling::bind(config, controller.clone(), effects, port, notices)
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
                combine(
                    result,
                    tokio::time::timeout(Duration::from_secs(3), runner)
                        .await
                        .map_err(failure)
                        .and_then(|joined| joined.map_err(failure))
                        .and_then(|served| served),
                )
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
        fixture
            .controller
            .execute(
                Credentials::new("app".into(), "test-password".into())?,
                None,
                bridge()?,
            )
            .await?;
        fixture.apply(playback()?).await?;
        fixture.registers().await
    })
    .await
}

#[tokio::test]
async fn a_recording_reservation_with_its_prompt_leaves_the_turn_free() -> Result<(), ControlError>
{
    silent(async |fixture: &mut Silent| {
        fixture
            .controller
            .execute(
                Credentials::new("app".into(), "test-password".into())?,
                None,
                bridge()?,
            )
            .await?;
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
        fixture
            .controller
            .execute(
                Credentials::new("app".into(), "test-password".into())?,
                None,
                bridge()?,
            )
            .await?;
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
        let _subscription = fixture
            .controller
            .subscribe(
                Credentials::new("app".into(), "test-password".into())?,
                None,
                "app".into(),
            )
            .await?;
        let key = fixture
            .controller
            .incoming(IncomingCall {
                id: id("digits")?,
                endpoint: "phone".into(),
                caller: "100".into(),
                context: "internal".into(),
                extension: "100".into(),
                target: IncomingTarget::Stasis {
                    application: "app".into(),
                    arguments: Vec::new(),
                },
            })
            .await?;
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
