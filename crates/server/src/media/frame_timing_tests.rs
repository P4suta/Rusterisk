use super::*;
use rusterisk_api::ChannelId;
use rusterisk_core::{MediaTaskKind, MediaTasks};
use std::future::Future;
use tokio::time::Instant;

const IDLE: Duration = Duration::from_millis(1010);

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

fn key() -> Result<CallKey, MediaError> {
    Ok(CallKey {
        id: ChannelId::try_from("timed".to_owned()).map_err(failure)?,
        generation: 3,
    })
}

struct Fixture {
    media: MediaPort,
    hub: MediaHub,
    _notices: MediaNotices,
    _stopper: watch::Sender<bool>,
    stop: watch::Receiver<bool>,
    frames: FrameClock,
    tasks: MediaTasks,
}

impl Fixture {
    fn new() -> Self {
        let (media, hub, notices) = MediaHub::new(4);
        let (stopper, stop) = watch::channel(false);
        Self {
            media,
            hub,
            _notices: notices,
            _stopper: stopper,
            stop,
            frames: FrameClock::new(),
            tasks: MediaTasks::new(3),
        }
    }

    async fn turn_within(&mut self, limit: Duration) -> Result<Option<Duration>, MediaError> {
        let started = Instant::now();
        match tokio::time::timeout(limit, self.hub.turn(&mut self.stop, &mut self.frames)).await {
            Ok(turn) => match turn? {
                Turn::Continue => Ok(Some(started.elapsed())),
                Turn::Stop => Err(failure("the hub stopped serving")),
            },
            Err(_) => Ok(None),
        }
    }

    async fn sleeps(&mut self) -> Result<(), MediaError> {
        match self.turn_within(IDLE).await? {
            None => Ok(()),
            Some(elapsed) => Err(failure(format!(
                "an idle hub woke after {elapsed:?} without work"
            ))),
        }
    }

    async fn ticks_one_period_later(&mut self) -> Result<(), MediaError> {
        match self.turn_within(FRAME_PERIOD * 2).await? {
            Some(elapsed) if elapsed == FRAME_PERIOD => Ok(()),
            woke => Err(failure(format!(
                "expected the next frame after {FRAME_PERIOD:?}, observed {woke:?}"
            ))),
        }
    }

    async fn close(mut self) -> Result<(), MediaError> {
        self.hub.close().await
    }
}

#[tokio::test(start_paused = true)]
async fn an_idle_hub_is_not_woken_by_frame_timing() -> Result<(), MediaError> {
    let (_media, mut hub, _notices) = MediaHub::new(4);
    let (_stopper, mut stop) = watch::channel(false);
    let mut polls = 0_u32;
    {
        let mut serve = std::pin::pin!(hub.serve(&mut stop));
        let counted = std::future::poll_fn(|context| {
            polls += 1;
            serve.as_mut().poll(context)
        });
        tokio::select! {
            biased;
            () = tokio::time::sleep(IDLE) => {}
            _served = counted => return Err(failure("the idle hub stopped serving")),
        }
    }
    assert_eq!(polls, 1, "an idle hub woke for frame timing");
    hub.close().await
}

#[tokio::test(start_paused = true)]
async fn a_port_attached_after_idleness_receives_its_first_frame_one_period_later(
) -> Result<(), MediaError> {
    let mut fixture = Fixture::new();
    fixture.sleeps().await?;
    let key = key()?;
    let (port, mut channels) =
        Port::new(PeerMediaDirection::SendReceive, DigitSupport::Unsupported);
    fixture.hub.install_link(key.clone(), port, Gate::Open)?;
    assert!(channels.outgoing.try_recv().is_err());
    fixture.ticks_one_period_later().await?;
    let frame = channels.outgoing.try_recv().map_err(failure)?;
    assert_eq!(frame.samples(), [0; 960]);
    fixture.hub.command(Command::Release { key })?;
    fixture.sleeps().await?;
    fixture.close().await
}

#[tokio::test(start_paused = true)]
async fn a_probe_without_a_port_keeps_frame_timing_until_it_closes() -> Result<(), MediaError> {
    let mut fixture = Fixture::new();
    fixture.sleeps().await?;
    let progress = fixture.media.test_signal(key()?).map_err(failure)?;
    assert_eq!(fixture.turn_within(IDLE).await?, Some(Duration::ZERO));
    fixture.ticks_one_period_later().await?;
    drop(progress);
    fixture.ticks_one_period_later().await?;
    fixture.sleeps().await?;
    fixture.close().await
}

#[tokio::test(start_paused = true)]
async fn a_bridge_playback_without_a_port_keeps_frame_timing_until_it_ends(
) -> Result<(), MediaError> {
    let mut fixture = Fixture::new();
    fixture.sleeps().await?;
    let token = fixture
        .tasks
        .reserve(MediaTaskKind::Playback)
        .map_err(|_| MediaError::Capacity)?;
    let (completed, mut completion) = oneshot::channel();
    let (reply, mut response) = oneshot::channel();
    fixture.hub.command(Command::ReservePlayback {
        token,
        target: AudioTarget::Bridge(1),
        completed,
        options: Default::default(),
        reply,
    })?;
    response.try_recv().map_err(failure)??;
    fixture.ticks_one_period_later().await?;
    fixture.ticks_one_period_later().await?;
    fixture.hub.command(Command::ControlPlayback {
        token,
        operation: PlaybackControl::Stop,
    })?;
    completion.try_recv().map_err(failure)??;
    fixture.sleeps().await?;
    fixture.close().await
}

#[tokio::test(start_paused = true)]
async fn a_bridge_recording_without_a_port_keeps_frame_timing_until_it_ends(
) -> Result<(), MediaError> {
    let mut fixture = Fixture::new();
    fixture.sleeps().await?;
    let token = fixture
        .tasks
        .reserve(MediaTaskKind::Recording)
        .map_err(|_| MediaError::Capacity)?;
    let (completed, mut completion) = oneshot::channel();
    let (reply, mut response) = oneshot::channel();
    fixture.hub.command(Command::ReserveRecording {
        token,
        target: RecordingTarget::Bridge(1),
        completed,
        reply,
    })?;
    response.try_recv().map_err(failure)??;
    fixture.ticks_one_period_later().await?;
    fixture.ticks_one_period_later().await?;
    fixture.hub.command(Command::EndRecording {
        token,
        mode: RecordingEnd::Cancel,
    })?;
    completion.try_recv().map_err(failure)?.map_err(failure)?;
    fixture.sleeps().await?;
    fixture.close().await
}
