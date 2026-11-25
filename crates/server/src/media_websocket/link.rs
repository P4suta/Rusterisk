use super::{BufferItem, BufferNotice, FlowNotice, MediaBuffer, MediaSocket, SocketMessage};
use crate::clock::Clock;
use crate::media_link::{Channels, DigitSchedule, DigitStep, LinkStats};
use rusterisk_api::{
    ChannelId, MediaCommand, MediaCommandError, MediaControlFormat, MediaDirection, MediaEvent,
    MediaFormat,
};
use rusterisk_media::{alaw_encode, mulaw_encode, Frame, MediaError, OpusCodec};
use std::{collections::BTreeMap, time::Duration};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct MediaLinkOptions {
    pub connection_id: ChannelId,
    pub channel_id: ChannelId,
    pub channel: String,
    pub format: MediaFormat,
    pub control_format: MediaControlFormat,
    pub direction: MediaDirection,
    pub auto_answer: bool,
    pub channel_variables: BTreeMap<String, String>,
}

pub struct WebSocketMediaLink<S> {
    socket: S,
    channels: Channels,
    options: MediaLinkOptions,
    audio: MediaBuffer,
    encoder: Option<OpusCodec>,
    answer: mpsc::Sender<()>,
}

enum ClosedBy {
    Local,
    Peer,
}

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

impl<S: MediaSocket> WebSocketMediaLink<S> {
    pub fn new(
        socket: S,
        channels: Channels,
        options: MediaLinkOptions,
        answer: mpsc::Sender<()>,
    ) -> Result<Self, MediaError> {
        if options.channel.len() > 256
            || options.channel_variables.len() > 128
            || options
                .channel_variables
                .iter()
                .any(|(name, value)| name.len() > 128 || value.len() > 4096)
        {
            return Err(MediaError::Capacity);
        }
        let audio = MediaBuffer::new(options.format)?;
        let encoder = (options.format == MediaFormat::Opus)
            .then(OpusCodec::new)
            .transpose()?;
        Ok(Self {
            socket,
            channels,
            options,
            audio,
            encoder,
            answer,
        })
    }

    async fn send(&mut self, message: SocketMessage) -> Result<(), MediaError> {
        tokio::time::timeout(Duration::from_millis(100), self.socket.send(message))
            .await
            .map_err(failure)?
    }

    async fn event(&mut self, event: MediaEvent) -> Result<(), MediaError> {
        let text = event.encode(self.options.control_format).map_err(failure)?;
        if text.len() > 65500 {
            return Err(MediaError::Capacity);
        }
        self.send(SocketMessage::Text(text)).await
    }

    async fn flow(&mut self, flow: Option<FlowNotice>) -> Result<(), MediaError> {
        if let Some(flow) = flow {
            let channel_id = self.options.channel_id.clone();
            self.event(match flow {
                FlowNotice::Xoff => MediaEvent::MediaXoff { channel_id },
                FlowNotice::Xon => MediaEvent::MediaXon { channel_id },
            })
            .await?;
        }
        Ok(())
    }

    async fn command(&mut self, command: MediaCommand) -> Result<bool, MediaError> {
        let queue_command = match &command {
            MediaCommand::StartMediaBuffering => Some("START_MEDIA_BUFFERING"),
            MediaCommand::StopMediaBuffering { .. } => Some("STOP_MEDIA_BUFFERING"),
            MediaCommand::MarkMedia { .. } => Some("MARK_MEDIA"),
            MediaCommand::FlushMedia => Some("FLUSH_MEDIA"),
            MediaCommand::ReportQueueDrained => Some("REPORT_QUEUE_DRAINED"),
            MediaCommand::PauseMedia => Some("PAUSE_MEDIA"),
            MediaCommand::ContinueMedia => Some("CONTINUE_MEDIA"),
            MediaCommand::SetMediaDirection { .. } => Some("SET_MEDIA_DIRECTION"),
            MediaCommand::Answer | MediaCommand::Hangup | MediaCommand::GetStatus => None,
        };
        if let Some(name) = queue_command.filter(|_| self.options.format == MediaFormat::Opus) {
            self.event(MediaEvent::Error {
                channel_id: self.options.channel_id.clone(),
                error_text: format!("{name} not supported in passthrough mode"),
            })
            .await?;
            return Ok(false);
        }
        let restricted = match &command {
            MediaCommand::StartMediaBuffering => Some("START_MEDIA_BUFFERING"),
            MediaCommand::StopMediaBuffering { .. } => Some("STOP_MEDIA_BUFFERING"),
            MediaCommand::MarkMedia { .. } => Some("MARK_MEDIA"),
            MediaCommand::PauseMedia => Some("PAUSE_MEDIA"),
            MediaCommand::ContinueMedia => Some("CONTINUE_MEDIA"),
            MediaCommand::Answer
            | MediaCommand::Hangup
            | MediaCommand::FlushMedia
            | MediaCommand::GetStatus
            | MediaCommand::ReportQueueDrained
            | MediaCommand::SetMediaDirection { .. } => None,
        };
        if let Some(name) = restricted.filter(|_| self.options.direction == MediaDirection::In) {
            self.event(MediaEvent::Error {
                channel_id: self.options.channel_id.clone(),
                error_text: format!("{name} not supported while media direction is 'in'"),
            })
            .await?;
            return Ok(false);
        }
        match command {
            MediaCommand::Answer => self.answer.try_send(()).map_err(|_| MediaError::Capacity)?,
            MediaCommand::Hangup => return Ok(true),
            MediaCommand::StartMediaBuffering => self.audio.start_buffering(),
            MediaCommand::StopMediaBuffering { correlation_id } => {
                let flow = self
                    .audio
                    .finish_buffering(correlation_id.unwrap_or_default())?;
                self.flow(flow).await?;
            }
            MediaCommand::MarkMedia { correlation_id } => {
                self.audio.mark(correlation_id.unwrap_or_default())?
            }
            MediaCommand::FlushMedia => {
                let flow = self.audio.flush();
                self.flow(flow).await?;
            }
            MediaCommand::PauseMedia => self.audio.pause(),
            MediaCommand::ContinueMedia => self.audio.resume(),
            MediaCommand::ReportQueueDrained => self.audio.report_drained(),
            MediaCommand::SetMediaDirection { direction } => self.options.direction = direction,
            MediaCommand::GetStatus => {
                self.event(MediaEvent::Status {
                    channel_id: self.options.channel_id.clone(),
                    queue_length: self.audio.queued_frames() as u16,
                    xon_level: 800,
                    xoff_level: 900,
                    queue_full: self.audio.is_full(),
                    bulk_media: self.audio.is_buffering(),
                    media_paused: self.audio.is_paused(),
                })
                .await?
            }
        }
        Ok(false)
    }

    async fn tick(&mut self, stats: &mut LinkStats) -> Result<(), MediaError> {
        for _ in 0..129 {
            let output = self.audio.pop();
            self.flow(output.flow).await?;
            let audio = match output.item {
                Some(BufferItem::Frame(frame)) => {
                    match self.channels.incoming.try_send(frame) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => stats.dropped += 1,
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            return Err(MediaError::UnknownStream)
                        }
                    }
                    true
                }
                Some(BufferItem::Notice(notice)) => {
                    let channel_id = self.options.channel_id.clone();
                    self.event(match notice {
                        BufferNotice::Mark(correlation_id) => MediaEvent::MediaMarkProcessed {
                            channel_id,
                            correlation_id,
                        },
                        BufferNotice::BufferingCompleted(correlation_id) => {
                            MediaEvent::MediaBufferingCompleted {
                                channel_id,
                                correlation_id,
                            }
                        }
                    })
                    .await?;
                    false
                }
                None => {
                    if output.drained {
                        self.event(MediaEvent::QueueDrained {
                            channel_id: self.options.channel_id.clone(),
                        })
                        .await?;
                    }
                    break;
                }
            };
            if output.drained {
                self.event(MediaEvent::QueueDrained {
                    channel_id: self.options.channel_id.clone(),
                })
                .await?;
            }
            if audio || self.audio.is_empty() {
                break;
            }
        }
        Ok(())
    }

    fn encoded(&mut self, frame: Frame) -> Result<axum::body::Bytes, MediaError> {
        let frame = frame.resample(self.options.format.rate())?;
        let bytes: Vec<u8> = match self.options.format {
            MediaFormat::Pcmu => frame
                .samples()
                .iter()
                .map(|sample| mulaw_encode(*sample))
                .collect(),
            MediaFormat::Pcma => frame
                .samples()
                .iter()
                .map(|sample| alaw_encode(*sample))
                .collect(),
            MediaFormat::Pcm8
            | MediaFormat::Pcm12
            | MediaFormat::Pcm16
            | MediaFormat::Pcm24
            | MediaFormat::Pcm32
            | MediaFormat::Pcm44
            | MediaFormat::Pcm48 => frame
                .samples()
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect(),
            MediaFormat::Opus => self
                .encoder
                .as_mut()
                .ok_or(MediaError::InvalidRoute)?
                .encode(frame)?
                .to_vec(),
        };
        Ok(bytes.into())
    }

    async fn serve(&mut self) -> Result<(LinkStats, ClosedBy), MediaError> {
        let channel_variables = std::mem::take(&mut self.options.channel_variables);
        self.event(MediaEvent::MediaStart {
            connection_id: self.options.connection_id.clone(),
            channel: self.options.channel.clone(),
            channel_id: self.options.channel_id.clone(),
            format: self.options.format,
            optimal_frame_size: self.options.format.optimal_frame_size(),
            ptime: 20,
            channel_variables,
        })
        .await?;
        if self.options.auto_answer {
            self.answer.try_send(()).map_err(|_| MediaError::Capacity)?;
        }
        let mut stats = LinkStats::default();
        let period = Duration::from_millis(20);
        let started = tokio::time::Instant::now();
        let mut tick = tokio::time::interval_at(started + period, period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let clock = Clock::start();
        let mut digits = DigitSchedule::default();
        loop {
            if *self.channels.stop.borrow() || self.channels.incoming.is_closed() {
                break;
            }
            tokio::select! {
                changed = self.channels.stop.changed() => if changed.is_err() || *self.channels.stop.borrow() { break; },
                changed = self.channels.direction.changed() => { if changed.is_err() { break; } let flow = self.audio.flush(); self.flow(flow).await?; },
                message = self.socket.receive() => match message? {
                    None | Some(SocketMessage::Close) => return Ok((stats, ClosedBy::Peer)),
                    Some(SocketMessage::Ping(bytes)) => self.send(SocketMessage::Pong(bytes)).await?,
                    Some(SocketMessage::Pong(_)) => {},
                    Some(SocketMessage::Text(text)) => {
                        match MediaCommand::parse(&text, self.options.control_format) {
                            Ok(command) => match self.command(command).await {
                                Ok(true) => break,
                                Ok(false) => {},
                                Err(MediaError::Capacity) => self.event(MediaEvent::Error { channel_id: self.options.channel_id.clone(), error_text: "media capacity exceeded".into() }).await?,
                                Err(error @ (MediaError::InvalidFrame | MediaError::InvalidPacket | MediaError::InvalidRoute | MediaError::UnknownStream | MediaError::Unsupported | MediaError::Codec(_) | MediaError::Transport(_) | MediaError::Control(_))) => return Err(error),
                            },
                            Err(MediaCommandError::UnsupportedFormat) => self.event(MediaEvent::Error { channel_id: self.options.channel_id.clone(), error_text: "SET_MEDIA_DIRECTION only supports JSON format.\n".into() }).await?,
                            Err(MediaCommandError::Capacity | MediaCommandError::Invalid | MediaCommandError::Json(_)) => tracing::debug!(channel = %self.options.channel_id, "media command refused"),
                        }
                    }
                    Some(SocketMessage::Binary(bytes)) => {
                        if bytes.len() > 65500 { return Err(MediaError::Capacity); }
                        if self.options.direction != MediaDirection::In && self.channels.direction.borrow().can_send() {
                            match self.audio.push(&bytes) {
                                Ok(flow) => { stats.packets_received += 1; self.flow(flow).await?; },
                                Err(MediaError::Capacity) => stats.dropped += 1,
                                Err(error @ (MediaError::InvalidFrame | MediaError::InvalidPacket | MediaError::InvalidRoute | MediaError::UnknownStream | MediaError::Unsupported | MediaError::Codec(_) | MediaError::Transport(_) | MediaError::Control(_))) => return Err(error),
                            }
                        }
                    }
                },
                frame = self.channels.outgoing.recv() => {
                    let Some(frame) = frame else { break; };
                    if self.options.direction != MediaDirection::Out && self.channels.direction.borrow().can_receive() { let bytes = self.encoded(frame)?; self.send(SocketMessage::Binary(bytes)).await?; stats.packets_sent += 1; }
                }
                Some(command) = self.channels.commands.recv(), if digits.idle() => digits.admit(command)?,
                () = clock.reached(digits.wake()) => {
                    match digits.advance(clock.now_ms().map_err(failure)?, true)? {
                        DigitStep::Send(tone) => self.event(MediaEvent::DtmfEnd {
                            channel_id: self.options.channel_id.clone(),
                            digit: tone.digit.symbol(),
                        }).await?,
                        DigitStep::Finished(token) => self.channels.completed.try_send(token).map_err(|_| MediaError::Capacity)?,
                        DigitStep::Waiting => {}
                    }
                }
                _instant = tick.tick() => {
                    if self.options.direction != MediaDirection::In { self.tick(&mut stats).await?; }
                }
            }
        }
        Ok((stats, ClosedBy::Local))
    }

    pub async fn run(mut self) -> Result<LinkStats, MediaError> {
        let result = self.serve().await;
        let cleanup = match &result {
            Ok((_, ClosedBy::Peer)) => {
                tokio::time::timeout(Duration::from_millis(100), self.socket.flush())
                    .await
                    .map_err(failure)?
            }
            Ok((_, ClosedBy::Local)) | Err(_) => self.send(SocketMessage::Close).await,
        };
        match (result, cleanup) {
            (Ok((stats, _)), Ok(())) => Ok(stats),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(failure(format!(
                "{primary}; media socket cleanup failed: {cleanup}"
            ))),
        }
    }
}
