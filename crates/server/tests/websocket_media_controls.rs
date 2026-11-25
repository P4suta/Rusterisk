use rusterisk_api::{
    ChannelId, MediaCommand, MediaControlFormat, MediaDirection, MediaEvent, MediaFormat,
};
use rusterisk_core::{DtmfDigit, DtmfOutbox, DtmfTiming, DtmfToken, DtmfTone, PeerMediaDirection};
use rusterisk_media::{Frame, MediaError};
use rusterisk_server::{
    media_link::{Channels, DigitCommand, LinkStats},
    media_websocket::{MediaLinkOptions, MediaSocket, SocketMessage, WebSocketMediaLink},
};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

struct Socket {
    input: mpsc::Receiver<SocketMessage>,
    output: mpsc::Sender<SocketMessage>,
}
impl MediaSocket for Socket {
    async fn receive(&mut self) -> Result<Option<SocketMessage>, MediaError> {
        Ok(self.input.recv().await)
    }
    async fn send(&mut self, message: SocketMessage) -> Result<(), MediaError> {
        self.output
            .try_send(message)
            .map_err(|_| MediaError::Capacity)
    }
    async fn flush(&mut self) -> Result<(), MediaError> {
        Ok(())
    }
}

struct Harness {
    peer: mpsc::Sender<SocketMessage>,
    messages: mpsc::Receiver<SocketMessage>,
    frames: mpsc::Receiver<Frame>,
    commands: mpsc::Sender<DigitCommand>,
    stop: watch::Sender<bool>,
    _direction: watch::Sender<PeerMediaDirection>,
    _outgoing: mpsc::Sender<Frame>,
    _digits: mpsc::Receiver<DtmfTone>,
    completed: mpsc::Receiver<DtmfToken>,
    _answers: mpsc::Receiver<()>,
    task: tokio::task::JoinHandle<Result<LinkStats, MediaError>>,
}

impl Harness {
    fn start(format: MediaControlFormat) -> Result<Self, MediaError> {
        Self::with_media(format, MediaFormat::Pcmu)
    }

    fn with_media(format: MediaControlFormat, media: MediaFormat) -> Result<Self, MediaError> {
        let (peer, input) = mpsc::channel(64);
        let (output, messages) = mpsc::channel(64);
        let (incoming, frames) = mpsc::channel(3);
        let (outgoing, sent) = mpsc::channel(3);
        let (digits, received_digits) = mpsc::channel(16);
        let (commands, command_input) = mpsc::channel(8);
        let (completed, completions) = mpsc::channel(8);
        let (stop, stopped) = watch::channel(false);
        let (direction, directed) = watch::channel(PeerMediaDirection::SendReceive);
        let (answer, answers) = mpsc::channel(8);
        let id = ChannelId::try_from("channel".to_owned()).map_err(failure)?;
        let link = WebSocketMediaLink::new(
            Socket { input, output },
            Channels {
                incoming,
                digits,
                outgoing: sent,
                commands: command_input,
                completed,
                stop: stopped,
                direction: directed,
            },
            MediaLinkOptions {
                connection_id: id.clone(),
                channel_id: id,
                channel: "WebSocket/INCOMING/channel".into(),
                format: media,
                control_format: format,
                direction: MediaDirection::Both,
                auto_answer: true,
                channel_variables: Default::default(),
            },
            answer,
        )?;
        Ok(Self {
            peer,
            messages,
            frames,
            commands,
            stop,
            _direction: direction,
            _outgoing: outgoing,
            _digits: received_digits,
            completed: completions,
            _answers: answers,
            task: tokio::spawn(link.run()),
        })
    }

    async fn text(&mut self) -> Result<String, MediaError> {
        let message = tokio::time::timeout(Duration::from_secs(1), self.messages.recv())
            .await
            .map_err(failure)?
            .ok_or(MediaError::UnknownStream)?;
        match message {
            SocketMessage::Text(text) => Ok(text),
            SocketMessage::Binary(_)
            | SocketMessage::Ping(_)
            | SocketMessage::Pong(_)
            | SocketMessage::Close => Err(MediaError::InvalidPacket),
        }
    }

    async fn command(&self, command: MediaCommand) -> Result<(), MediaError> {
        self.peer
            .send(SocketMessage::Text(
                command.encode(MediaControlFormat::Json).map_err(failure)?,
            ))
            .await
            .map_err(failure)
    }

    async fn status(&mut self) -> Result<MediaEvent, MediaError> {
        self.command(MediaCommand::GetStatus).await?;
        serde_json::from_str(&self.text().await?).map_err(failure)
    }

    async fn close(self) -> Result<(), MediaError> {
        self.stop.send_replace(true);
        self.task.await.map_err(failure)??;
        Ok(())
    }
}

#[tokio::test]
async fn packet_audio_rejects_queue_commands_with_the_reference_error() -> Result<(), MediaError> {
    let mut harness = Harness::with_media(MediaControlFormat::Json, MediaFormat::Opus)?;
    let result = async {
        let start: MediaEvent = serde_json::from_str(&harness.text().await?).map_err(failure)?;
        assert!(matches!(start, MediaEvent::MediaStart { format: MediaFormat::Opus, optimal_frame_size: 0, .. }));
        for (name, command) in [
            ("START_MEDIA_BUFFERING", MediaCommand::StartMediaBuffering),
            ("STOP_MEDIA_BUFFERING", MediaCommand::StopMediaBuffering { correlation_id: None }),
            ("MARK_MEDIA", MediaCommand::MarkMedia { correlation_id: None }),
            ("FLUSH_MEDIA", MediaCommand::FlushMedia),
            ("REPORT_QUEUE_DRAINED", MediaCommand::ReportQueueDrained),
            ("PAUSE_MEDIA", MediaCommand::PauseMedia),
            ("CONTINUE_MEDIA", MediaCommand::ContinueMedia),
            ("SET_MEDIA_DIRECTION", MediaCommand::SetMediaDirection { direction: MediaDirection::In }),
        ] {
            harness.command(command).await?;
            let event: MediaEvent = serde_json::from_str(&harness.text().await?).map_err(failure)?;
            assert!(matches!(event, MediaEvent::Error { error_text, .. } if error_text == format!("{name} not supported in passthrough mode")));
        }
        assert!(matches!(harness.status().await?, MediaEvent::Status { queue_length: 0, media_paused: false, bulk_media: false, .. }));
        Ok(())
    }.await;
    harness.close().await?;
    result
}

#[tokio::test(start_paused = true)]
async fn digits_and_completion_follow_their_deadlines_between_audio_frames(
) -> Result<(), MediaError> {
    let mut harness = Harness::start(MediaControlFormat::Json)?;
    harness.text().await?;
    let digits = [DtmfDigit::Five, DtmfDigit::Hash];
    let command = DtmfOutbox::new(u16::MAX)
        .and_then(|mut outbox| outbox.admit(&digits, DtmfTiming::new(7, 3, 4, 5)?))
        .map_err(|error| failure(format!("{error:?}")))?;
    let token = command.token;
    let started = tokio::time::Instant::now();
    harness
        .commands
        .try_send(DigitCommand::Sequence(command))
        .map_err(failure)?;
    for (digit, at) in [('5', 7), ('#', 14)] {
        let event = serde_json::from_str::<MediaEvent>(&harness.text().await?).map_err(failure)?;
        assert!(matches!(event, MediaEvent::DtmfEnd { digit: heard, .. } if heard == digit));
        assert_eq!(started.elapsed(), Duration::from_millis(at));
    }
    assert_eq!(harness.completed.recv().await, Some(token));
    assert_eq!(started.elapsed(), Duration::from_millis(23));
    harness.close().await
}

#[tokio::test]
async fn plain_text_direction_commands_return_the_reference_error() -> Result<(), MediaError> {
    let mut harness = Harness::start(MediaControlFormat::PlainText)?;
    let result = async {
        assert!(harness.text().await?.starts_with("MEDIA_START "));
        for text in ["SET_MEDIA_DIRECTION both", "SET_MEDIA_DIRECTION sideways", "SET_MEDIA_DIRECTION"] {
            harness.peer.send(SocketMessage::Text(text.into())).await.map_err(failure)?;
            assert_eq!(harness.text().await?, "ERROR channel_id:channel error_text:SET_MEDIA_DIRECTION only supports JSON format.\n");
        }
        Ok(())
    }.await;
    harness.close().await?;
    result
}

#[tokio::test]
async fn receive_only_direction_keeps_queued_audio_while_digits_continue() -> Result<(), MediaError>
{
    let mut harness = Harness::start(MediaControlFormat::Json)?;
    let result = async {
        harness.text().await?;
        harness.command(MediaCommand::PauseMedia).await?;
        harness.peer.send(SocketMessage::Binary(vec![0xff; 32000].into())).await.map_err(failure)?;
        assert!(matches!(harness.status().await?, MediaEvent::Status { queue_length: 200, media_paused: true, .. }));
        harness.command(MediaCommand::ContinueMedia).await?;
        harness.command(MediaCommand::SetMediaDirection { direction: MediaDirection::In }).await?;
        let MediaEvent::Status { queue_length: before, .. } = harness.status().await? else { return Err(MediaError::InvalidPacket); };
        while harness.frames.try_recv().is_ok() {}
        harness.commands.send(DigitCommand::Tone(DtmfTone { digit: DtmfDigit::Hash, duration_ms: 100 })).await.map_err(failure)?;
        assert!(matches!(serde_json::from_str::<MediaEvent>(&harness.text().await?).map_err(failure)?, MediaEvent::DtmfEnd { digit: '#', .. }));
        assert!(matches!(harness.status().await?, MediaEvent::Status { queue_length, .. } if queue_length == before));
        assert!(matches!(harness.frames.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        harness.command(MediaCommand::SetMediaDirection { direction: MediaDirection::Both }).await?;
        assert!(tokio::time::timeout(Duration::from_secs(1), harness.frames.recv()).await.map_err(failure)?.is_some());
        Ok(())
    }.await;
    harness.close().await?;
    result
}
