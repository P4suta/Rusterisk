use crate::audiosocket::AudioSocketLink;
use crate::rtp::{LinkStats, RtpChannels, RtpDigitCommand, RtpLink, RtpSession, SocketPair};
use crate::storage::{MusicCursor, PlaybackSource, RecordingSink, WorkerError};
use crate::CallKey;
use rusterisk_core::{DtmfCommand, DtmfToken, DtmfTone, MediaTaskToken, PeerMediaDirection};
use rusterisk_media::{AudioTarget, Frame, Gate, MediaError, Mixer, TelephoneFormat};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;
#[cfg(test)]
mod admission_tests;
mod measurements;
mod playback;
mod recording;
mod streams;
mod webrtc;
pub use playback::PlaybackControl;
use playback::Playbacks;
use recording::Recordings;
pub use recording::{RecordingEnd, RecordingPrompt, RecordingTarget};

pub enum MediaNotice {
    Answer {
        key: CallKey,
    },
    DtmfFinished {
        key: CallKey,
        token: DtmfToken,
    },
    Dtmf {
        key: CallKey,
        tone: DtmfTone,
    },
    Connected {
        key: CallKey,
    },
    Stopped {
        key: CallKey,
        result: Result<LinkStats, MediaError>,
    },
}

#[derive(Clone, Copy)]
pub(crate) enum AttachmentMode {
    Answered,
    Early,
}

impl AttachmentMode {
    fn gate(self, direction: PeerMediaDirection) -> Gate {
        match self {
            Self::Early => Gate::Closed,
            Self::Answered => match direction {
                PeerMediaDirection::SendReceive => Gate::Open,
                PeerMediaDirection::SendOnly => Gate::OutgoingMuted,
                PeerMediaDirection::ReceiveOnly => Gate::IncomingMuted,
                PeerMediaDirection::Inactive => Gate::Closed,
            },
        }
    }
}

pub(crate) struct AttachmentReceipt {
    response: oneshot::Receiver<Result<(), MediaError>>,
}

impl AttachmentReceipt {
    pub(crate) async fn wait(self) -> Result<(), MediaError> {
        tokio::time::timeout(Duration::from_secs(5), self.response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))?
    }
}

enum Command {
    Measurements {
        key: CallKey,
        reply: oneshot::Sender<Option<crate::rtp::Measurements>>,
    },
    ReceivedDigit {
        key: CallKey,
        digit: rusterisk_core::DtmfDigit,
    },
    ReservePlayback {
        token: MediaTaskToken,
        target: AudioTarget<CallKey>,
        completed: oneshot::Sender<Result<(), MediaError>>,
        options: rusterisk_api::PlaybackOptions,
        reply: oneshot::Sender<Result<(), MediaError>>,
    },
    ReserveMusic {
        key: CallKey,
        token: MediaTaskToken,
        class: String,
        completed: oneshot::Sender<Result<(), MediaError>>,
        reply: oneshot::Sender<Result<Option<MusicCursor>, MediaError>>,
    },
    AttachPlayback {
        token: MediaTaskToken,
        source: PlaybackSource,
        reply: oneshot::Sender<Result<(), MediaError>>,
    },
    ControlPlayback {
        token: MediaTaskToken,
        operation: PlaybackControl,
    },
    ReserveRecording {
        token: MediaTaskToken,
        target: RecordingTarget,
        completed: oneshot::Sender<Result<(), WorkerError>>,
        reply: oneshot::Sender<Result<(), MediaError>>,
    },
    AttachRecording {
        token: MediaTaskToken,
        sink: RecordingSink,
        options: rusterisk_api::RecordingOptions,
        prompt: Option<RecordingPrompt>,
        reply: oneshot::Sender<Result<(), MediaError>>,
    },
    EndRecording {
        token: MediaTaskToken,
        mode: RecordingEnd,
    },
    ControlRecording {
        token: MediaTaskToken,
        operation: rusterisk_core::RecordingControl,
        reply: oneshot::Sender<Result<(), MediaError>>,
    },
    Digits {
        key: CallKey,
        command: RtpDigitCommand,
        reply: oneshot::Sender<Result<(), MediaError>>,
    },
    Attach {
        key: CallKey,
        sockets: SocketPair,
        session: RtpSession,
        direction: PeerMediaDirection,
        mode: AttachmentMode,
        reply: oneshot::Sender<Result<(), MediaError>>,
    },
    AttachAudioSocket {
        key: CallKey,
        socket: tokio::net::TcpStream,
        rate: u32,
        reply: oneshot::Sender<Result<(), MediaError>>,
    },
    Release {
        key: CallKey,
    },
    Bridge {
        id: u64,
        members: Vec<CallKey>,
    },
    ReleaseBridge {
        id: u64,
    },
    Gate {
        key: CallKey,
        gate: Gate,
        generated_audio_gate: Gate,
    },
    PeerDirection {
        key: CallKey,
        direction: PeerMediaDirection,
        gate: Gate,
        generated_audio_gate: Gate,
        reply: oneshot::Sender<Result<bool, MediaError>>,
    },
}

#[derive(Clone)]
pub struct MediaPort {
    commands: mpsc::Sender<Command>,
    websocket: mpsc::Sender<streams::WebSocketAttachment<axum::extract::ws::WebSocket>>,
    client_websocket: mpsc::Sender<streams::WebSocketAttachment<streams::ClientSocket>>,
    webrtc: mpsc::Sender<webrtc::Attachment>,
}

impl MediaPort {
    pub(crate) async fn reserve_music(
        &self,
        key: CallKey,
        token: MediaTaskToken,
        class: String,
    ) -> Result<
        (
            Option<MusicCursor>,
            oneshot::Receiver<Result<(), MediaError>>,
        ),
        MediaError,
    > {
        let (completed, completion) = oneshot::channel();
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::ReserveMusic {
                key,
                token,
                class,
                completed,
                reply,
            })
            .map_err(|_| MediaError::Capacity)?;
        let cursor = tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))??;
        Ok((cursor, completion))
    }
    pub async fn received_digit(
        &self,
        key: CallKey,
        digit: rusterisk_core::DtmfDigit,
    ) -> Result<(), MediaError> {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.commands.send(Command::ReceivedDigit { key, digit }),
        )
        .await
        .map_err(|error| MediaError::Transport(error.to_string()))?
        .map_err(|_| MediaError::Capacity)
    }

    pub async fn play(
        &self,
        target: AudioTarget<CallKey>,
        token: MediaTaskToken,
        source: PlaybackSource,
    ) -> Result<oneshot::Receiver<Result<(), MediaError>>, MediaError> {
        let completion = self.reserve_playback(target, token).await?;
        self.attach_playback(token, source).await?;
        Ok(completion)
    }

    pub async fn reserve_playback(
        &self,
        target: AudioTarget<CallKey>,
        token: MediaTaskToken,
    ) -> Result<oneshot::Receiver<Result<(), MediaError>>, MediaError> {
        self.reserve_playback_with_options(target, token, Default::default())
            .await
    }

    pub async fn reserve_playback_with_options(
        &self,
        target: AudioTarget<CallKey>,
        token: MediaTaskToken,
        options: rusterisk_api::PlaybackOptions,
    ) -> Result<oneshot::Receiver<Result<(), MediaError>>, MediaError> {
        let (completed, completion) = oneshot::channel();
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::ReservePlayback {
                token,
                target,
                completed,
                options,
                reply,
            })
            .map_err(|_| MediaError::Capacity)?;
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))??;
        Ok(completion)
    }

    pub async fn attach_playback(
        &self,
        token: MediaTaskToken,
        source: PlaybackSource,
    ) -> Result<(), MediaError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::AttachPlayback {
                token,
                source,
                reply,
            })
            .map_err(|_| MediaError::Capacity)?;
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))?
    }

    pub fn stop_playback(&self, token: MediaTaskToken) -> Result<(), MediaError> {
        self.control_playback(token, PlaybackControl::Stop)
    }

    pub fn control_playback(
        &self,
        token: MediaTaskToken,
        operation: PlaybackControl,
    ) -> Result<(), MediaError> {
        self.commands
            .try_send(Command::ControlPlayback { token, operation })
            .map_err(|_| MediaError::Capacity)
    }

    pub async fn record(
        &self,
        target: RecordingTarget,
        token: MediaTaskToken,
        sink: RecordingSink,
    ) -> Result<oneshot::Receiver<Result<(), WorkerError>>, MediaError> {
        let completion = self.reserve_recording(target, token).await?;
        self.attach_recording(token, sink).await?;
        Ok(completion)
    }

    pub async fn reserve_recording(
        &self,
        target: RecordingTarget,
        token: MediaTaskToken,
    ) -> Result<oneshot::Receiver<Result<(), WorkerError>>, MediaError> {
        let (completed, completion) = oneshot::channel();
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::ReserveRecording {
                token,
                target,
                completed,
                reply,
            })
            .map_err(|_| MediaError::Capacity)?;
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))??;
        Ok(completion)
    }

    pub async fn attach_recording(
        &self,
        token: MediaTaskToken,
        sink: RecordingSink,
    ) -> Result<(), MediaError> {
        self.attach_recording_with_options(token, sink, Default::default(), None)
            .await
    }

    pub async fn attach_recording_with_options(
        &self,
        token: MediaTaskToken,
        sink: RecordingSink,
        options: rusterisk_api::RecordingOptions,
        prompt: Option<RecordingPrompt>,
    ) -> Result<(), MediaError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::AttachRecording {
                token,
                sink,
                options,
                prompt,
                reply,
            })
            .map_err(|_| MediaError::Capacity)?;
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))?
    }

    pub async fn control_recording(
        &self,
        token: MediaTaskToken,
        operation: rusterisk_core::RecordingControl,
    ) -> Result<(), MediaError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::ControlRecording {
                token,
                operation,
                reply,
            })
            .map_err(|_| MediaError::Capacity)?;
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))?
    }

    pub fn stop_recording(
        &self,
        token: MediaTaskToken,
        mode: RecordingEnd,
    ) -> Result<(), MediaError> {
        self.commands
            .try_send(Command::EndRecording { token, mode })
            .map_err(|_| MediaError::Capacity)
    }

    pub async fn send_tone(&self, key: CallKey, tone: DtmfTone) -> Result<(), MediaError> {
        if !(1..=5000).contains(&tone.duration_ms) {
            return Err(MediaError::InvalidPacket);
        }
        self.send_digits(key, RtpDigitCommand::Tone(tone)).await
    }

    pub async fn send_sequence(
        &self,
        key: CallKey,
        command: DtmfCommand,
    ) -> Result<(), MediaError> {
        self.send_digits(key, RtpDigitCommand::Sequence(command))
            .await
    }

    async fn send_digits(&self, key: CallKey, command: RtpDigitCommand) -> Result<(), MediaError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::Digits {
                key,
                command,
                reply,
            })
            .map_err(|_| MediaError::Capacity)?;
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))?
    }
    pub async fn attach(
        &self,
        key: CallKey,
        sockets: SocketPair,
        session: RtpSession,
        direction: PeerMediaDirection,
    ) -> Result<(), MediaError> {
        self.queue_attachment(key, sockets, session, direction, AttachmentMode::Answered)?
            .wait()
            .await
    }

    pub(crate) fn queue_attachment(
        &self,
        key: CallKey,
        sockets: SocketPair,
        session: RtpSession,
        direction: PeerMediaDirection,
        mode: AttachmentMode,
    ) -> Result<AttachmentReceipt, MediaError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::Attach {
                key,
                sockets,
                session,
                direction,
                mode,
                reply,
            })
            .map_err(|_| MediaError::Capacity)?;
        Ok(AttachmentReceipt { response })
    }
    pub fn release(&self, key: CallKey) -> Result<(), MediaError> {
        self.commands
            .try_send(Command::Release { key })
            .map_err(|_| MediaError::Capacity)
    }
    pub async fn attach_audiosocket(
        &self,
        key: CallKey,
        socket: tokio::net::TcpStream,
        rate: u32,
    ) -> Result<(), MediaError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::AttachAudioSocket {
                key,
                socket,
                rate,
                reply,
            })
            .map_err(|_| MediaError::Capacity)?;
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))?
    }
    pub fn bridge(&self, id: u64, members: Vec<CallKey>) -> Result<(), MediaError> {
        if members.len() > 64 {
            return Err(MediaError::Capacity);
        }
        self.commands
            .try_send(Command::Bridge { id, members })
            .map_err(|_| MediaError::Capacity)
    }
    pub fn release_bridge(&self, id: u64) -> Result<(), MediaError> {
        self.commands
            .try_send(Command::ReleaseBridge { id })
            .map_err(|_| MediaError::Capacity)
    }
    pub fn gate(&self, key: CallKey, gate: Gate) -> Result<(), MediaError> {
        self.gate_with_generated_audio(key, gate, gate)
    }
    pub(crate) fn gate_with_generated_audio(
        &self,
        key: CallKey,
        gate: Gate,
        generated_audio_gate: Gate,
    ) -> Result<(), MediaError> {
        self.commands
            .try_send(Command::Gate {
                key,
                gate,
                generated_audio_gate,
            })
            .map_err(|_| MediaError::Capacity)
    }
    pub async fn peer_direction(
        &self,
        key: CallKey,
        direction: PeerMediaDirection,
        gate: Gate,
    ) -> Result<bool, MediaError> {
        self.peer_direction_with_generated_audio(key, direction, gate, gate)
            .await
    }

    pub(crate) async fn peer_direction_with_generated_audio(
        &self,
        key: CallKey,
        direction: PeerMediaDirection,
        gate: Gate,
        generated_audio_gate: Gate,
    ) -> Result<bool, MediaError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::PeerDirection {
                key,
                direction,
                gate,
                generated_audio_gate,
                reply,
            })
            .map_err(|_| MediaError::Capacity)?;
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))?
    }
}

enum Connection {
    Waiting,
    Connected,
}
enum DigitSupport {
    Unsupported,
    Rfc4733(TelephoneFormat),
    Stream,
}
struct SavedMusic {
    class: String,
    cursor: MusicCursor,
}
struct Port {
    measurements: Option<watch::Receiver<crate::rtp::Measurements>>,
    connection_lease: Option<crate::MediaConnectionLease>,
    answers: Option<mpsc::Receiver<()>>,
    incoming: mpsc::Receiver<Frame>,
    digits: mpsc::Receiver<DtmfTone>,
    outgoing: mpsc::Sender<Frame>,
    commands: mpsc::Sender<RtpDigitCommand>,
    completed: mpsc::Receiver<DtmfToken>,
    digit_support: DigitSupport,
    stop: watch::Sender<bool>,
    direction: watch::Sender<PeerMediaDirection>,
    generated_audio_gate: Gate,
    saved_music: Option<SavedMusic>,
    connection: Connection,
}

pub struct MediaHub {
    capacity: usize,
    commands: mpsc::Receiver<Command>,
    websocket: mpsc::Receiver<streams::WebSocketAttachment<axum::extract::ws::WebSocket>>,
    client_websocket: mpsc::Receiver<streams::WebSocketAttachment<streams::ClientSocket>>,
    webrtc: mpsc::Receiver<webrtc::Attachment>,
    notices: mpsc::Sender<MediaNotice>,
    mixer: Mixer<CallKey>,
    ports: BTreeMap<CallKey, Port>,
    workers: JoinSet<(CallKey, Result<LinkStats, MediaError>)>,
    dropped: u64,
    recordings: Recordings,
    playbacks: Playbacks,
}

impl MediaHub {
    fn remember_music(&mut self) {
        for (key, class, cursor) in self.playbacks.music_cursors() {
            if let Some(port) = self.ports.get_mut(key) {
                match &mut port.saved_music {
                    Some(saved) if saved.class.eq_ignore_ascii_case(class) => saved.cursor = cursor,
                    saved => {
                        *saved = Some(SavedMusic {
                            class: class.to_owned(),
                            cursor,
                        })
                    }
                }
            }
        }
    }
    pub fn new(capacity: usize) -> (MediaPort, Self, mpsc::Receiver<MediaNotice>) {
        let (commands, receiver) = mpsc::channel(128);
        let (websocket, websocket_receiver) = mpsc::channel(8);
        let (client_websocket, client_websocket_receiver) = mpsc::channel(8);
        let (webrtc, webrtc_receiver) = mpsc::channel(8);
        let (notices, events) = mpsc::channel(128);
        (
            MediaPort {
                commands,
                websocket,
                client_websocket,
                webrtc,
            },
            Self {
                capacity: capacity.min(64),
                commands: receiver,
                websocket: websocket_receiver,
                client_websocket: client_websocket_receiver,
                webrtc: webrtc_receiver,
                notices,
                mixer: Mixer::new(capacity),
                ports: BTreeMap::new(),
                workers: JoinSet::new(),
                dropped: 0,
                recordings: Recordings::new(),
                playbacks: Playbacks::new(),
            },
            events,
        )
    }

    fn notice(&self, notice: MediaNotice) -> Result<(), MediaError> {
        self.notices
            .try_send(notice)
            .map_err(|_| MediaError::Capacity)
    }

    fn remove(&mut self, key: &CallKey) -> Result<(), MediaError> {
        self.recordings.release(key);
        self.playbacks.release(key);
        if let Some(port) = self.ports.remove(key) {
            port.stop.send_replace(true);
            if !self.mixer.remove(key) {
                return Err(MediaError::InvalidRoute);
            }
        }
        Ok(())
    }

    fn attach(
        &mut self,
        key: CallKey,
        sockets: SocketPair,
        session: RtpSession,
        initial_direction: PeerMediaDirection,
        mode: AttachmentMode,
    ) -> Result<(), MediaError> {
        self.check_link(&key)?;
        let support = session
            .telephone
            .map(DigitSupport::Rfc4733)
            .unwrap_or(DigitSupport::Unsupported);
        let (mut port, channels) = Port::new(initial_direction, support);
        let link = RtpLink::new(sockets, session, channels)?;
        port.measurements = Some(link.observations());
        self.install_link(key.clone(), port, mode.gate(initial_direction))?;
        self.workers.spawn(async move { (key, link.run().await) });
        Ok(())
    }

    fn complete_attachment(
        &mut self,
        key: &CallKey,
        result: Result<(), MediaError>,
        reply: oneshot::Sender<Result<(), MediaError>>,
    ) -> Result<(), MediaError> {
        let admitted = result.is_ok();
        if reply.send(result).is_err() && admitted {
            self.remove(key)?;
        }
        Ok(())
    }

    fn command(&mut self, command: Command) -> Result<(), MediaError> {
        self.remember_music();
        match command {
            Command::Measurements { key, reply } => {
                let sample = self
                    .ports
                    .get(&key)
                    .and_then(|port| port.measurements.as_ref())
                    .filter(|reader| reader.has_changed().is_ok())
                    .map(|reader| reader.borrow().clone());
                if reply.send(sample).is_err() {
                    tracing::debug!("RTP statistics requester disconnected");
                }
            }
            Command::ReceivedDigit { key, digit } => {
                if self.ports.contains_key(&key) {
                    self.recordings
                        .received_digit(&key, self.mixer.bridge_for(&key), digit);
                }
            }
            Command::ReservePlayback {
                token,
                target,
                completed,
                options,
                reply,
            } => {
                if !reply.is_closed() {
                    let valid = match &target {
                        AudioTarget::Channel(key) => self.ports.contains_key(key),
                        AudioTarget::Bridge(_) => true,
                    };
                    let result = if valid {
                        self.playbacks.reserve(token, target, completed, options)
                    } else {
                        Err(MediaError::UnknownStream)
                    };
                    let admitted = result.is_ok();
                    if reply.send(result).is_err() && admitted {
                        self.playbacks.end(token, Ok(()));
                    }
                }
            }
            Command::ReserveMusic {
                key,
                token,
                class,
                completed,
                reply,
            } => {
                if !reply.is_closed() {
                    let result = match self.ports.get(&key) {
                        Some(port) => {
                            let cursor = port
                                .saved_music
                                .as_ref()
                                .filter(|saved| saved.class.eq_ignore_ascii_case(&class))
                                .map(|saved| saved.cursor);
                            self.playbacks
                                .reserve_music(token, key, class, completed)
                                .map(|()| cursor)
                        }
                        None => Err(MediaError::UnknownStream),
                    };
                    let admitted = result.is_ok();
                    if reply.send(result).is_err() && admitted {
                        self.playbacks.end(token, Ok(()));
                    }
                }
            }
            Command::AttachPlayback {
                token,
                source,
                reply,
            } => {
                if !reply.is_closed() {
                    let result = self.playbacks.attach(token, source);
                    let admitted = result.is_ok();
                    if reply.send(result).is_err() && admitted {
                        self.playbacks.end(token, Ok(()));
                    }
                }
            }
            Command::ControlPlayback { token, operation } => {
                self.playbacks.control(token, operation)
            }
            Command::ReserveRecording {
                token,
                target,
                completed,
                reply,
            } => {
                if !reply.is_closed() {
                    let valid = match &target {
                        RecordingTarget::Channel(key) => self.ports.contains_key(key),
                        RecordingTarget::Bridge(_) => true,
                    };
                    let result = if valid {
                        self.recordings.reserve(token, target, completed)
                    } else {
                        Err(MediaError::UnknownStream)
                    };
                    let admitted = result.is_ok();
                    if reply.send(result).is_err() && admitted {
                        self.recordings.end(token, RecordingEnd::Cancel);
                    }
                }
            }
            Command::AttachRecording {
                token,
                sink,
                options,
                prompt,
                reply,
            } => {
                if !reply.is_closed() {
                    let result = self.recordings.attach(token, sink, options, prompt);
                    let admitted = result.is_ok();
                    if reply.send(result).is_err() && admitted {
                        self.recordings.end(token, RecordingEnd::Cancel);
                    }
                }
            }
            Command::EndRecording { token, mode } => self.recordings.end(token, mode),
            Command::ControlRecording {
                token,
                operation,
                reply,
            } => {
                if !reply.is_closed() {
                    let result = self.recordings.control(token, operation);
                    if reply.send(result).is_err() {
                        tracing::debug!(?token, "recording control receiver disconnected");
                    }
                }
            }
            Command::Digits {
                key,
                command,
                reply,
            } => {
                if !reply.is_closed() {
                    let result = self
                        .ports
                        .get(&key)
                        .ok_or(MediaError::UnknownStream)
                        .and_then(|port| {
                            let events = match port.digit_support {
                                DigitSupport::Unsupported => return Err(MediaError::Unsupported),
                                DigitSupport::Rfc4733(format) => format.events(),
                                DigitSupport::Stream => u16::MAX,
                            };
                            let supported = match &command {
                                RtpDigitCommand::Tone(tone) => {
                                    events & (1 << tone.digit.event()) != 0
                                }
                                RtpDigitCommand::Sequence(command) => {
                                    command.sequence.supported_by(events)
                                }
                            };
                            if !supported {
                                return Err(MediaError::Unsupported);
                            }
                            port.commands
                                .try_send(command)
                                .map_err(|_| MediaError::Capacity)
                        });
                    if reply.send(result).is_err() {
                        tracing::debug!(channel = %key.id, generation = key.generation, "tone admission receiver closed");
                    }
                }
            }
            Command::Attach {
                key,
                sockets,
                session,
                direction,
                mode,
                reply,
            } => {
                if !reply.is_closed() {
                    let result = self.attach(key.clone(), sockets, session, direction, mode);
                    self.complete_attachment(&key, result, reply)?;
                }
            }
            Command::Release { key } => self.remove(&key)?,
            Command::AttachAudioSocket {
                key,
                socket,
                rate,
                reply,
            } => {
                if !reply.is_closed() {
                    let result = self.attach_audiosocket(key.clone(), socket, rate);
                    self.complete_attachment(&key, result, reply)?;
                }
            }
            Command::ReleaseBridge { id } => {
                self.playbacks.release_bridge(id);
                self.recordings.release_bridge(id);
                self.mixer.bridge(id, &[])?;
            }
            Command::Bridge { id, members } => {
                let mut active = Vec::with_capacity(members.len());
                for key in members {
                    if self.ports.contains_key(&key) {
                        active.push(key);
                    } else {
                        self.notice(MediaNotice::Stopped {
                            key,
                            result: Err(MediaError::UnknownStream),
                        })?;
                    }
                }
                self.mixer.bridge(id, &active)?;
            }
            Command::Gate {
                key,
                gate,
                generated_audio_gate,
            } => {
                if let Some(port) = self.ports.get_mut(&key) {
                    self.mixer.gates(&key, gate, generated_audio_gate)?;
                    port.generated_audio_gate = generated_audio_gate;
                }
            }
            Command::PeerDirection {
                key,
                direction,
                gate,
                generated_audio_gate,
                reply,
            } => {
                if !reply.is_closed() {
                    let result = if let Some(port) = self.ports.get_mut(&key) {
                        self.mixer
                            .gates(&key, gate, generated_audio_gate)
                            .map(|()| {
                                port.generated_audio_gate = generated_audio_gate;
                                port.direction.send_replace(direction);
                                true
                            })
                    } else {
                        Ok(false)
                    };
                    if reply.send(result).is_err() {
                        tracing::debug!(channel = %key.id, generation = key.generation, "media direction receiver closed");
                    }
                }
            }
        }
        Ok(())
    }

    fn tick(&mut self) -> Result<(), MediaError> {
        for (key, port) in &mut self.ports {
            if port
                .connection_lease
                .as_ref()
                .is_some_and(crate::MediaConnectionLease::is_released)
            {
                port.stop.send_replace(true);
                continue;
            }
            if let Some(answers) = &mut port.answers {
                for _ in 0..8 {
                    match answers.try_recv() {
                        Ok(()) => self
                            .notices
                            .try_send(MediaNotice::Answer { key: key.clone() })
                            .map_err(|_| MediaError::Capacity)?,
                        Err(
                            mpsc::error::TryRecvError::Empty
                            | mpsc::error::TryRecvError::Disconnected,
                        ) => break,
                    }
                }
            }
            for _ in 0..8 {
                match port.completed.try_recv() {
                    Ok(token) => self
                        .notices
                        .try_send(MediaNotice::DtmfFinished {
                            key: key.clone(),
                            token,
                        })
                        .map_err(|_| MediaError::Capacity)?,
                    Err(
                        mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected,
                    ) => break,
                }
            }
            for _ in 0..16 {
                match port.digits.try_recv() {
                    Ok(tone) => {
                        self.recordings
                            .received_digit(key, self.mixer.bridge_for(key), tone.digit);
                        self.notices
                            .try_send(MediaNotice::Dtmf {
                                key: key.clone(),
                                tone,
                            })
                            .map_err(|_| MediaError::Capacity)?;
                    }
                    Err(
                        mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected,
                    ) => break,
                }
            }
            for _ in 0..3 {
                match port.incoming.try_recv() {
                    Ok(frame) => {
                        self.recordings.channel(key, frame);
                        match self.mixer.push(key, frame) {
                            Ok(()) => {
                                if matches!(port.connection, Connection::Waiting) {
                                    port.connection = Connection::Connected;
                                    self.notices
                                        .try_send(MediaNotice::Connected { key: key.clone() })
                                        .map_err(|_| MediaError::Capacity)?;
                                }
                            }
                            Err(_) => self.dropped += 1,
                        }
                    }
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        port.stop.send_replace(true);
                        break;
                    }
                }
            }
        }
        self.remember_music();
        let frames = self.playbacks.tick()?;
        let mut playback = Vec::with_capacity(frames.len());
        let mut music = Vec::with_capacity(frames.len());
        for (target, frame) in frames {
            if self.playbacks.is_music(&target) {
                let AudioTarget::Channel(key) = target else {
                    return Err(MediaError::InvalidRoute);
                };
                let port = self.ports.get(&key).ok_or(MediaError::UnknownStream)?;
                music.push((key, port.generated_audio_gate, frame));
            } else {
                playback.push((target, frame));
            }
        }
        let audio = self.mixer.tick_with_music(&playback, &music)?;
        self.recordings.bridges(&audio.bridges)?;
        for (key, frame) in audio.channels {
            let port = self.ports.get(&key).ok_or(MediaError::UnknownStream)?;
            match port.outgoing.try_send(frame) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => self.dropped += 1,
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    port.stop.send_replace(true);
                }
            }
        }
        Ok(())
    }

    async fn serve(&mut self, stop: &mut watch::Receiver<bool>) -> Result<(), MediaError> {
        let period = Duration::from_millis(20);
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                changed = stop.changed() => if changed.is_err() || *stop.borrow() { break; },
                command = self.commands.recv() => match command {
                    Some(command) => self.command(command)?,
                    None => break,
                },
                attachment = self.websocket.recv() => match attachment {
                    Some(attachment) => self.websocket_attachment(attachment)?,
                    None => break,
                },
                attachment = self.client_websocket.recv() => match attachment {
                    Some(attachment) => self.websocket_attachment(attachment)?,
                    None => break,
                },
                attachment = self.webrtc.recv() => match attachment {
                    Some(attachment) => self.webrtc_attachment(attachment)?,
                    None => break,
                },
                result = self.workers.join_next(), if !self.workers.is_empty() => {
                    let (key, result) = result.ok_or(MediaError::InvalidRoute)?.map_err(|error| MediaError::Transport(error.to_string()))?;
                    if self.ports.contains_key(&key) {
                        self.remove(&key)?;
                        self.notice(MediaNotice::Stopped { key, result })?;
                    }
                }
                result = self.recordings.workers.join_next(), if !self.recordings.workers.is_empty() => {
                    result.ok_or(MediaError::InvalidRoute)?.map_err(|error| MediaError::Transport(error.to_string()))?;
                }
                _instant = tick.tick() => self.tick()?,
            }
        }
        Ok(())
    }

    async fn close(&mut self) -> Result<(), MediaError> {
        self.playbacks.close();
        for (_, port) in std::mem::take(&mut self.ports) {
            port.stop.send_replace(true);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut failure = self.recordings.close().await.err();
        while !self.workers.is_empty() {
            match tokio::time::timeout_at(deadline, self.workers.join_next()).await {
                Ok(Some(Ok((_, Ok(_))))) | Ok(None) => {}
                Ok(Some(Ok((_key, Err(error))))) => {
                    tracing::error!(%error, "media worker failed during shutdown");
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
                Ok(Some(Err(error))) => {
                    tracing::error!(%error, "media task failed during shutdown");
                    if failure.is_none() {
                        failure = Some(MediaError::Transport(error.to_string()));
                    }
                }
                Err(error) => {
                    self.workers.abort_all();
                    while let Some(joined) = self.workers.join_next().await {
                        match joined {
                            Ok((_, Ok(_))) => {}
                            Ok((_, Err(error))) => {
                                tracing::error!(%error, "media worker failed before cancellation")
                            }
                            Err(error) => {
                                tracing::debug!(%error, "media worker cancelled during shutdown")
                            }
                        }
                    }
                    return Err(MediaError::Transport(error.to_string()));
                }
            }
        }
        match failure {
            None => Ok(()),
            Some(error) => Err(error),
        }
    }

    pub async fn run(mut self, mut stop: watch::Receiver<bool>) -> Result<(), MediaError> {
        let result = self.serve(&mut stop).await;
        let cleanup = self.close().await;
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(MediaError::Transport(format!(
                "{primary}; media cleanup failed: {cleanup}"
            ))),
        }
    }
}
