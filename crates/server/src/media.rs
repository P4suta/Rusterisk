use crate::audiosocket::AudioSocketLink;
use crate::rtp::{LinkStats, RtpChannels, RtpDigitCommand, RtpLeg, RtpSession, SocketPair};
use crate::storage::{MusicCursor, PlaybackSource, RecordingSink, WorkerError};
use crate::CallKey;
use rusterisk_core::{DtmfCommand, DtmfToken, DtmfTone, MediaTaskToken, PeerMediaDirection};
use rusterisk_media::{AudioTarget, Frame, Gate, MediaError, Mixer, TelephoneFormat, MAX_PACKET};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;
#[cfg(test)]
mod admission_tests;
#[cfg(test)]
mod frame_timing_tests;
#[cfg(test)]
mod leg_tests;
mod measurements;
mod notices;
pub use notices::MediaNotices;
use notices::NoticeSender;
mod orders;
mod probe;
#[cfg(test)]
mod queued;
#[cfg(test)]
mod release_tests;
pub(crate) use probe::Progress as ProbeProgress;
#[cfg(test)]
pub(crate) use queued::Queued;
mod playback;
mod recording;
mod stream_frames;
mod streams;
mod webrtc;
pub(crate) use orders::Order;
pub use playback::PlaybackControl;
use playback::Playbacks;
use recording::Recordings;
pub use recording::{RecordingEnd, RecordingPrompt, RecordingTarget};

pub enum MediaNotice {
    Released {
        key: CallKey,
    },
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
    // The answer to an attachment that its sender numbered and queued without a reply.
    Attached {
        key: CallKey,
        serial: u64,
        result: Result<(), MediaError>,
    },
    // The answer to a direction change that its sender numbered and queued without a reply.
    DirectionApplied {
        key: CallKey,
        serial: u64,
        result: Result<bool, MediaError>,
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

// The media task's answer to a command that its sender queued without waiting.
pub(crate) struct Receipt<T> {
    response: oneshot::Receiver<Result<T, MediaError>>,
}

pub(crate) type AttachmentReceipt = Receipt<()>;

// A queued reservation: the answer to its admission and the end of the admitted operation.
pub(crate) struct Reservation<T, E> {
    pub(crate) admission: Receipt<T>,
    pub(crate) completed: oneshot::Receiver<Result<(), E>>,
}

impl<T> Receipt<T> {
    pub(crate) async fn wait(self) -> Result<T, MediaError> {
        tokio::time::timeout(Duration::from_secs(5), self.response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))?
    }
}

enum Command {
    Probe {
        key: CallKey,
        challenge: probe::Challenge,
    },
    Measurements {
        key: CallKey,
        reply: oneshot::Sender<Option<(String, crate::rtp::Measurements)>>,
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
    },
    Digits {
        key: CallKey,
        command: RtpDigitCommand,
        reply: Option<oneshot::Sender<Result<(), MediaError>>>,
    },
    Attach {
        key: CallKey,
        sockets: SocketPair,
        session: RtpSession,
        direction: PeerMediaDirection,
        mode: AttachmentMode,
        reply: Completion<()>,
    },
    AttachWebRtc(usize),
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
        reply: Completion<bool>,
    },
}

// How the media task answers a queued command.
// A notice carries the number that its sender gave the command.
pub(crate) enum Completion<T> {
    Reply(oneshot::Sender<Result<T, MediaError>>),
    Notice(u64, mpsc::OwnedPermit<()>),
}

impl<T> Completion<T> {
    // A sender that dropped its reply no longer wants the command.
    fn abandoned(&self) -> bool {
        match self {
            Self::Reply(reply) => reply.is_closed(),
            Self::Notice(..) => false,
        }
    }
}

pub(crate) struct AttachmentRelease {
    pub(crate) serial: u64,
    command: mpsc::OwnedPermit<Command>,
}

impl AttachmentRelease {
    pub(crate) fn send(self, order: Order) {
        self.command.send(order.command());
    }
}

#[derive(Clone)]
pub struct MediaPort {
    commands: mpsc::Sender<Command>,
    notices: mpsc::WeakSender<()>,
    websocket: mpsc::Sender<streams::WebSocketAttachment<axum::extract::ws::WebSocket>>,
    client_websocket: mpsc::Sender<streams::WebSocketAttachment<streams::ClientSocket>>,
    webrtc: [std::sync::mpsc::SyncSender<webrtc::Attachment>; webrtc::SLOTS],
}

impl MediaPort {
    fn completion<T>(&self, serial: u64) -> Result<Completion<T>, MediaError> {
        self.notices
            .upgrade()
            .ok_or(MediaError::ServiceClosed)?
            .try_reserve_owned()
            .map(|slot| Completion::Notice(serial, slot))
            .map_err(notices::service_queue_error)
    }

    pub(crate) fn reserve_attachment_release(
        &self,
        serial: u64,
    ) -> Result<AttachmentRelease, MediaError> {
        let command = self
            .commands
            .clone()
            .try_reserve_owned()
            .map_err(notices::service_queue_error)?;
        Ok(AttachmentRelease { serial, command })
    }

    pub(crate) fn queue_music(
        &self,
        key: CallKey,
        token: MediaTaskToken,
        class: String,
    ) -> Result<Reservation<Option<MusicCursor>, MediaError>, MediaError> {
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
            .map_err(notices::service_queue_error)?;
        Ok(Reservation {
            admission: Receipt { response },
            completed: completion,
        })
    }
    pub fn received_digit(
        &self,
        key: CallKey,
        digit: rusterisk_core::DtmfDigit,
    ) -> Result<(), MediaError> {
        self.commands
            .try_send(Command::ReceivedDigit { key, digit })
            .map_err(notices::service_queue_error)
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
        let reservation = self.queue_playback(target, token, options)?;
        reservation.admission.wait().await?;
        Ok(reservation.completed)
    }

    pub(crate) fn queue_playback(
        &self,
        target: AudioTarget<CallKey>,
        token: MediaTaskToken,
        options: rusterisk_api::PlaybackOptions,
    ) -> Result<Reservation<(), MediaError>, MediaError> {
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
            .map_err(notices::service_queue_error)?;
        Ok(Reservation {
            admission: Receipt { response },
            completed: completion,
        })
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
            .map_err(notices::service_queue_error)?;
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
            .map_err(notices::service_queue_error)
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
        let reservation = self.queue_recording(target, token)?;
        reservation.admission.wait().await?;
        Ok(reservation.completed)
    }

    pub(crate) fn queue_recording(
        &self,
        target: RecordingTarget,
        token: MediaTaskToken,
    ) -> Result<Reservation<(), WorkerError>, MediaError> {
        let (completed, completion) = oneshot::channel();
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::ReserveRecording {
                token,
                target,
                completed,
                reply,
            })
            .map_err(notices::service_queue_error)?;
        Ok(Reservation {
            admission: Receipt { response },
            completed: completion,
        })
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
            .map_err(notices::service_queue_error)?;
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))?
    }

    pub fn control_recording(
        &self,
        token: MediaTaskToken,
        operation: rusterisk_core::RecordingControl,
    ) -> Result<(), MediaError> {
        self.commands
            .try_send(Command::ControlRecording { token, operation })
            .map_err(notices::service_queue_error)
    }

    pub fn stop_recording(
        &self,
        token: MediaTaskToken,
        mode: RecordingEnd,
    ) -> Result<(), MediaError> {
        self.send(Order::EndRecording { token, mode })
    }

    pub async fn send_tone(&self, key: CallKey, tone: DtmfTone) -> Result<(), MediaError> {
        if !(1..=5000).contains(&tone.duration_ms) {
            return Err(MediaError::InvalidPacket);
        }
        self.send_digits(key, RtpDigitCommand::Tone(tone)).await
    }

    // The media task reports a refused sequence as a finished digit command.
    pub fn send_sequence(&self, key: CallKey, command: DtmfCommand) -> Result<(), MediaError> {
        self.commands
            .try_send(Command::Digits {
                key,
                command: RtpDigitCommand::Sequence(command),
                reply: None,
            })
            .map_err(notices::service_queue_error)
    }

    async fn send_digits(&self, key: CallKey, command: RtpDigitCommand) -> Result<(), MediaError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::Digits {
                key,
                command,
                reply: Some(reply),
            })
            .map_err(notices::service_queue_error)?;
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
                reply: Completion::Reply(reply),
            })
            .map_err(notices::service_queue_error)?;
        Ok(AttachmentReceipt { response })
    }

    // The media task reports the result of this attachment as a notice with the same number.
    pub(crate) fn start_attachment(
        &self,
        key: CallKey,
        sockets: SocketPair,
        session: RtpSession,
        direction: PeerMediaDirection,
        mode: AttachmentMode,
        serial: u64,
    ) -> Result<AttachmentRelease, MediaError> {
        let release = self.reserve_attachment_release(serial)?;
        self.commands
            .try_send(Command::Attach {
                key,
                sockets,
                session,
                direction,
                mode,
                reply: self.completion(serial)?,
            })
            .map_err(notices::service_queue_error)?;
        Ok(release)
    }

    pub fn release(&self, key: CallKey) -> Result<(), MediaError> {
        self.send(Order::Release(key))
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
            .map_err(notices::service_queue_error)?;
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))?
    }
    pub fn bridge(&self, id: u64, members: Vec<CallKey>) -> Result<(), MediaError> {
        self.send(Order::bridge(id, members)?)
    }
    pub fn release_bridge(&self, id: u64) -> Result<(), MediaError> {
        self.send(Order::ReleaseBridge(id))
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
        self.send(Order::Gate {
            key,
            gate,
            generated_audio_gate,
        })
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
                reply: Completion::Reply(reply),
            })
            .map_err(notices::service_queue_error)?;
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
// The channels of a link that runs its own task.
struct TaskIo {
    audio: stream_frames::TaskFrames,
    digits: mpsc::Receiver<DtmfTone>,
    commands: mpsc::Sender<RtpDigitCommand>,
    completed: mpsc::Receiver<DtmfToken>,
    stop: watch::Sender<bool>,
    direction: watch::Sender<PeerMediaDirection>,
}

// How the media task exchanges audio with a call's link.
enum Io {
    Task(TaskIo),
    // A protocol leg that the media task runs in its own turn, which the hub keeps beside the port.
    Owned,
}

struct Port {
    connection_lease: Option<crate::MediaConnectionLease>,
    answers: Option<mpsc::Receiver<()>>,
    io: Io,
    digit_support: DigitSupport,
    generated_audio_gate: Gate,
    saved_music: Option<SavedMusic>,
    connection: Connection,
}

// The parts of the media task that take one owned leg's turn output.
struct Delivery<'a> {
    recordings: &'a mut Recordings,
    mixer: &'a mut Mixer<CallKey>,
    probes: &'a mut BTreeMap<CallKey, probe::Challenge>,
    notices: &'a NoticeSender,
    ports: &'a mut BTreeMap<CallKey, Port>,
    dropped: &'a mut u64,
}

impl Delivery<'_> {
    // Hands a leg's digits, finished commands, and frame onward, or records the leg's failure.
    fn deliver(
        &mut self,
        key: &CallKey,
        frame: Result<Option<Frame>, MediaError>,
        tones: &mut Vec<DtmfTone>,
        finished: &mut Vec<DtmfToken>,
        failed: &mut Vec<(CallKey, MediaError)>,
    ) -> Result<(), MediaError> {
        let frame = match frame {
            Ok(frame) => frame,
            Err(error) => {
                tones.clear();
                finished.clear();
                failed.push((key.clone(), error));
                return Ok(());
            }
        };
        for tone in tones.drain(..) {
            self.recordings
                .received_digit(key, self.mixer.bridge_for(key), tone.digit);
            self.notices.try_send(MediaNotice::Dtmf {
                key: key.clone(),
                tone,
            })?;
        }
        for token in finished.drain(..) {
            self.notices.try_send(MediaNotice::DtmfFinished {
                key: key.clone(),
                token,
            })?;
        }
        let Some(frame) = frame else {
            return Ok(());
        };
        if let Some(challenge) = self.probes.get_mut(key) {
            challenge.receive(&frame)?;
        }
        self.recordings.channel(key, frame);
        match self.mixer.push(key, frame) {
            Ok(()) => {
                if let Some(port) = self.ports.get_mut(key) {
                    if matches!(port.connection, Connection::Waiting) {
                        port.connection = Connection::Connected;
                        self.notices
                            .try_send(MediaNotice::Connected { key: key.clone() })?;
                    }
                }
            }
            Err(_) => *self.dropped += 1,
        }
        Ok(())
    }
}

const FRAME_PERIOD: Duration = Duration::from_millis(20);

enum Turn {
    Continue,
    Stop,
}

struct FrameClock {
    interval: tokio::time::Interval,
    running: bool,
}

impl FrameClock {
    fn new() -> Self {
        let mut interval =
            tokio::time::interval_at(tokio::time::Instant::now() + FRAME_PERIOD, FRAME_PERIOD);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Self {
            interval,
            running: false,
        }
    }

    fn follow(&mut self, needed: bool) -> bool {
        if needed && !self.running {
            self.interval.reset();
        }
        self.running = needed;
        needed
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamRelease {
    Running,
    Requested,
}

pub struct MediaHub {
    capacity: usize,
    commands: mpsc::Receiver<Command>,
    websocket: mpsc::Receiver<streams::WebSocketAttachment<axum::extract::ws::WebSocket>>,
    client_websocket: mpsc::Receiver<streams::WebSocketAttachment<streams::ClientSocket>>,
    webrtc: [std::sync::mpsc::Receiver<webrtc::Attachment>; webrtc::SLOTS],
    notices: NoticeSender,
    mixer: Mixer<CallKey>,
    ports: BTreeMap<CallKey, Port>,
    // The protocol and WebRTC legs that this task runs, each beside its port.
    legs: BTreeMap<CallKey, RtpLeg>,
    webrtc_legs: BTreeMap<CallKey, crate::webrtc::WebRtcLeg>,
    finished: Vec<DtmfToken>,
    // Room for one datagram, which every leg reads into in turn.
    scratch: [u8; MAX_PACKET + 1],
    tones: Vec<DtmfTone>,
    failed: Vec<(CallKey, MediaError)>,
    workers: JoinSet<(CallKey, Result<LinkStats, MediaError>)>,
    stream_releases: BTreeMap<rusterisk_core::CallHandle, StreamRelease>,
    release_notices: BTreeMap<u8, CallKey>,
    pub(crate) activity: crate::activity::Meter,
    dropped: u64,
    recordings: Recordings,
    playbacks: Playbacks,
    probes: BTreeMap<CallKey, probe::Challenge>,
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
    pub fn new(capacity: usize) -> (MediaPort, Self, MediaNotices) {
        let (commands, receiver) = mpsc::channel(128);
        let (websocket, websocket_receiver) = mpsc::channel(8);
        let (client_websocket, client_websocket_receiver) = mpsc::channel(8);
        let slots = std::array::from_fn(|_| std::sync::mpsc::sync_channel(1));
        let webrtc = slots.each_ref().map(|(sender, _)| sender.clone());
        let webrtc_receiver = slots.map(|(_, receiver)| receiver);
        let (notices, events) = notices::channel();
        (
            MediaPort {
                commands,
                notices: notices.completions.downgrade(),
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
                legs: BTreeMap::new(),
                webrtc_legs: BTreeMap::new(),
                finished: Vec::with_capacity(16),
                scratch: [0; MAX_PACKET + 1],
                tones: Vec::with_capacity(16),
                failed: Vec::with_capacity(4),
                workers: JoinSet::new(),
                stream_releases: BTreeMap::new(),
                release_notices: BTreeMap::new(),
                activity: crate::activity::Meter::default(),
                dropped: 0,
                recordings: Recordings::new(),
                playbacks: Playbacks::new(),
                probes: BTreeMap::new(),
            },
            events,
        )
    }

    fn notice(&self, notice: MediaNotice) -> Result<(), MediaError> {
        self.notices.try_send(notice)
    }

    fn release_notice(&mut self, key: CallKey) -> Result<(), MediaError> {
        match self.notices.ordinary.clone().try_reserve_owned() {
            Ok(slot) => self
                .notices
                .send_reserved(slot, MediaNotice::Released { key })?,
            Err(mpsc::error::TrySendError::Full(_)) => {
                let slot = key.handle().slot();
                if self
                    .release_notices
                    .get(&slot)
                    .is_none_or(|pending| pending.generation < key.generation)
                {
                    self.release_notices.insert(slot, key);
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return Err(MediaError::ServiceClosed),
        }
        Ok(())
    }

    fn release_call(&mut self, key: CallKey) -> Result<(), MediaError> {
        self.remove(&key)?;
        if let Some(stream) = self.stream_releases.get_mut(&key.handle()) {
            *stream = StreamRelease::Requested;
            return Ok(());
        }
        self.release_notice(key)
    }

    fn remove(&mut self, key: &CallKey) -> Result<(), MediaError> {
        self.probes.remove(key);
        self.recordings.release(key);
        self.playbacks.release(key);
        if let Some(port) = self.ports.remove(key) {
            match port.io {
                Io::Task(io) => {
                    io.stop.send_replace(true);
                }
                Io::Owned => {
                    if let Some(leg) = self.legs.remove(key) {
                        let stats = leg.finish();
                        tracing::debug!(channel = %key.id, generation = key.generation, ?stats, "media leg released");
                    }
                    if let Some(leg) = self.webrtc_legs.remove(key) {
                        let stats = leg.finish();
                        tracing::debug!(channel = %key.id, generation = key.generation, ?stats, "WebRTC leg released");
                    }
                }
            }
            if !self.mixer.remove(key) {
                return Err(MediaError::InvalidRoute);
            }
        }
        Ok(())
    }

    // Ends each WebRTC leg whose session closed, and tells signaling that its media stopped.
    fn end_closed_legs(&mut self) -> Result<(), MediaError> {
        while let Some(key) = self
            .webrtc_legs
            .iter()
            .find_map(|(key, leg)| leg.closed().then(|| key.clone()))
        {
            let stats = self
                .webrtc_legs
                .remove(&key)
                .map(crate::webrtc::WebRtcLeg::finish)
                .unwrap_or_default();
            self.remove(&key)?;
            self.notice(MediaNotice::Stopped {
                key,
                result: Ok(stats.audio),
            })?;
        }
        Ok(())
    }

    // Ends a leg that failed, and tells signaling that its media stopped.
    fn fail_legs(&mut self) -> Result<(), MediaError> {
        while let Some((key, error)) = self.failed.pop() {
            tracing::warn!(channel = %key.id, generation = key.generation, %error, "media leg failed");
            self.remove(&key)?;
            self.notice(MediaNotice::Stopped {
                key,
                result: Err(error),
            })?;
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
        let leg = RtpLeg::new(sockets, session, initial_direction)?;
        let port = Port::owned(initial_direction, support);
        self.install_link(
            key.clone(),
            port,
            mode.gate(initial_direction),
            session.format.rate(),
        )?;
        self.legs.insert(key, leg);
        Ok(())
    }

    fn complete(
        &mut self,
        key: &CallKey,
        result: Result<(), MediaError>,
        reply: Completion<()>,
    ) -> Result<(), MediaError> {
        match reply {
            Completion::Reply(reply) => self.complete_attachment(key, result, reply),
            Completion::Notice(serial, slot) => self.notices.send_reserved(
                slot,
                MediaNotice::Attached {
                    key: key.clone(),
                    serial,
                    result,
                },
            ),
        }
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
            Command::AttachWebRtc(slot) => {
                let attachment = self
                    .webrtc
                    .get(slot)
                    .ok_or(MediaError::InvalidRoute)?
                    .try_recv()
                    .map_err(|_| MediaError::InvalidRoute)?;
                self.webrtc_attachment(attachment)?;
            }
            Command::Probe { key, challenge } => {
                if self.probes.len() < self.capacity && !challenge.closed() {
                    self.probes.entry(key).or_insert(challenge);
                }
            }
            Command::Measurements { key, reply } => {
                let sample = self
                    .legs
                    .get(&key)
                    .map(|leg| (crate::utc_timestamps::now_millis(), leg.measurements()));
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
            Command::ControlRecording { token, operation } => {
                if let Err(error) = self.recordings.control(token, operation) {
                    tracing::warn!(?token, %error, "recording control failed");
                }
            }
            Command::Digits {
                key,
                command,
                reply,
            } => {
                if !reply.as_ref().is_some_and(oneshot::Sender::is_closed) {
                    let finished = match &command {
                        RtpDigitCommand::Sequence(command) => Some(command.token),
                        RtpDigitCommand::Tone(_) => None,
                    };
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
                            match &port.io {
                                Io::Task(io) => {
                                    io.commands.try_send(command).map_err(|error| match error {
                                        mpsc::error::TrySendError::Full(_) => MediaError::Capacity,
                                        mpsc::error::TrySendError::Closed(_) => {
                                            MediaError::UnknownStream
                                        }
                                    })
                                }
                                Io::Owned => {
                                    match (self.legs.get_mut(&key), self.webrtc_legs.get_mut(&key))
                                    {
                                        (Some(leg), _) => leg.queue_digits(command),
                                        (None, Some(leg)) => leg.queue_digits(command),
                                        (None, None) => Err(MediaError::UnknownStream),
                                    }
                                }
                            }
                        });
                    match (reply, result) {
                        (Some(reply), result) => {
                            if reply.send(result).is_err() {
                                tracing::debug!(channel = %key.id, generation = key.generation, "tone admission receiver closed");
                            }
                        }
                        (None, Ok(())) => {}
                        // A sender without a reply learns of a refusal as a finished digit command.
                        (None, Err(error)) => {
                            tracing::warn!(channel = %key.id, generation = key.generation, %error, "digit command refused");
                            if let Some(token) = finished {
                                self.notices
                                    .try_send(MediaNotice::DtmfFinished { key, token })?;
                            }
                        }
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
                if !reply.abandoned() {
                    let result = self.attach(key.clone(), sockets, session, direction, mode);
                    self.complete(&key, result, reply)?;
                }
            }
            Command::Release { key } => self.release_call(key)?,
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
                if !reply.abandoned() {
                    let result = if let Some(port) = self.ports.get_mut(&key) {
                        self.mixer
                            .gates(&key, gate, generated_audio_gate)
                            .map(|()| {
                                port.generated_audio_gate = generated_audio_gate;
                                match &port.io {
                                    Io::Task(io) => {
                                        io.direction.send_replace(direction);
                                    }
                                    Io::Owned => {
                                        if let Some(leg) = self.legs.get_mut(&key) {
                                            leg.set_direction(direction);
                                        }
                                        if let Some(leg) = self.webrtc_legs.get_mut(&key) {
                                            if let Err(error) = leg.set_direction(direction) {
                                                self.failed.push((key.clone(), error));
                                            }
                                        }
                                    }
                                }
                                true
                            })
                    } else {
                        Ok(false)
                    };
                    match reply {
                        Completion::Reply(reply) => {
                            if reply.send(result).is_err() {
                                tracing::debug!(channel = %key.id, generation = key.generation, "media direction receiver closed");
                            }
                        }
                        Completion::Notice(serial, slot) => {
                            self.notices.send_reserved(
                                slot,
                                MediaNotice::DirectionApplied {
                                    key,
                                    serial,
                                    result,
                                },
                            )?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn needs_frames(&self) -> bool {
        !self.ports.is_empty()
            || !self.probes.is_empty()
            || self.playbacks.needs_frames()
            || self.recordings.needs_frames()
    }

    fn tick(&mut self) -> Result<(), MediaError> {
        // Commands that signaling queued before this turn apply before the turn reads any audio.
        while let Ok(command) = self.commands.try_recv() {
            self.command(command)?;
        }
        self.probes.retain(|_, challenge| !challenge.closed());
        for (key, port) in &mut self.ports {
            // An owned leg has no channels, and the loop below runs it.
            let Io::Task(io) = &mut port.io else {
                continue;
            };
            if port
                .connection_lease
                .as_ref()
                .is_some_and(crate::MediaConnectionLease::is_released)
            {
                io.stop.send_replace(true);
                continue;
            }
            if let Some(answers) = &mut port.answers {
                for _ in 0..8 {
                    match answers.try_recv() {
                        Ok(()) => self
                            .notices
                            .try_send(MediaNotice::Answer { key: key.clone() })?,
                        Err(
                            mpsc::error::TryRecvError::Empty
                            | mpsc::error::TryRecvError::Disconnected,
                        ) => break,
                    }
                }
            }
            for _ in 0..8 {
                match io.completed.try_recv() {
                    Ok(token) => self.notices.try_send(MediaNotice::DtmfFinished {
                        key: key.clone(),
                        token,
                    })?,
                    Err(
                        mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected,
                    ) => break,
                }
            }
            for _ in 0..16 {
                match io.digits.try_recv() {
                    Ok(tone) => {
                        self.recordings
                            .received_digit(key, self.mixer.bridge_for(key), tone.digit);
                        self.notices.try_send(MediaNotice::Dtmf {
                            key: key.clone(),
                            tone,
                        })?;
                    }
                    Err(
                        mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected,
                    ) => break,
                }
            }
            match io.audio.receive() {
                Ok(frame) => {
                    if let Some(challenge) = self.probes.get_mut(key) {
                        challenge.receive(&frame)?;
                    }
                    self.recordings.channel(key, frame);
                    match self.mixer.push(key, frame) {
                        Ok(()) => {
                            if matches!(port.connection, Connection::Waiting) {
                                port.connection = Connection::Connected;
                                self.notices
                                    .try_send(MediaNotice::Connected { key: key.clone() })?;
                            }
                        }
                        Err(_) => self.dropped += 1,
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    io.stop.send_replace(true);
                }
            }
        }
        // Every owned leg reads what arrived and hands its next frame to the mixer before the mix.
        let mut delivery = Delivery {
            recordings: &mut self.recordings,
            mixer: &mut self.mixer,
            probes: &mut self.probes,
            notices: &self.notices,
            ports: &mut self.ports,
            dropped: &mut self.dropped,
        };
        for (key, leg) in &mut self.legs {
            let frame = leg
                .receive(&mut self.scratch, &mut self.tones, |_| Ok(()))
                .and_then(|()| leg.advance(&mut self.tones, &mut self.finished));
            delivery.deliver(
                key,
                frame,
                &mut self.tones,
                &mut self.finished,
                &mut self.failed,
            )?;
        }
        for (key, leg) in &mut self.webrtc_legs {
            let frame = leg
                .receive(&mut self.scratch)
                .and_then(|()| leg.advance(&mut self.tones, &mut self.finished));
            delivery.deliver(
                key,
                frame,
                &mut self.tones,
                &mut self.finished,
                &mut self.failed,
            )?;
        }
        self.end_closed_legs()?;
        self.fail_legs()?;
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
        for (key, frame) in &audio.channels {
            let port = self.ports.get(key).ok_or(MediaError::UnknownStream)?;
            let frame = match (self.probes.get_mut(key), port.generated_audio_gate) {
                (Some(probe), Gate::Open | Gate::IncomingMuted) => probe.transmit()?,
                _ => *frame,
            };
            match &port.io {
                Io::Task(io) => match io.audio.send(&frame) {
                    Ok(()) => {}
                    Err(MediaError::Capacity) => self.dropped += 1,
                    Err(MediaError::UnknownStream) => {
                        io.stop.send_replace(true);
                    }
                    Err(error) => self.failed.push((key.clone(), error)),
                },
                Io::Owned => {
                    let sent = match (self.legs.get_mut(key), self.webrtc_legs.get_mut(key)) {
                        (Some(leg), _) => leg.send(frame),
                        (None, Some(leg)) => leg.send(frame),
                        (None, None) => Ok(()),
                    };
                    if let Err(error) = sent {
                        self.failed.push((key.clone(), error));
                    }
                }
            }
        }
        self.fail_legs()
    }

    async fn serve(&mut self, stop: &mut watch::Receiver<bool>) -> Result<(), MediaError> {
        let mut frames = FrameClock::new();
        loop {
            match self.turn(stop, &mut frames).await? {
                Turn::Continue => {}
                Turn::Stop => return Ok(()),
            }
        }
    }

    async fn turn(
        &mut self,
        stop: &mut watch::Receiver<bool>,
        frames: &mut FrameClock,
    ) -> Result<Turn, MediaError> {
        if *stop.borrow() {
            return Ok(Turn::Stop);
        }
        let timed = frames.follow(self.needs_frames());
        let before = self.dropped;
        let mut late = false;
        tokio::select! {
            changed = stop.changed() => if changed.is_err() || *stop.borrow() { return Ok(Turn::Stop); },
            command = self.commands.recv() => match command {
                Some(command) => self.command(command)?,
                None => return Ok(Turn::Stop),
            },
            attachment = self.websocket.recv() => match attachment {
                Some(attachment) => self.websocket_attachment(attachment)?,
                None => return Ok(Turn::Stop),
            },
            attachment = self.client_websocket.recv() => match attachment {
                Some(attachment) => self.websocket_attachment(attachment)?,
                None => return Ok(Turn::Stop),
            },
            slot = self.notices.ordinary.clone().reserve_owned(), if !self.release_notices.is_empty() => {
                let slot = slot.map_err(|_| MediaError::ServiceClosed)?;
                if let Some((_, key)) = self.release_notices.pop_first() {
                    self.notices.send_reserved(slot, MediaNotice::Released { key })?;
                }
            }
            result = self.workers.join_next(), if !self.workers.is_empty() => {
                let (key, result) = result.ok_or(MediaError::InvalidRoute)?.map_err(|error| MediaError::Transport(error.to_string()))?;
                let release = self.stream_releases.remove(&key.handle());
                if release == Some(StreamRelease::Requested) {
                    self.release_notice(key)?;
                } else if self.ports.contains_key(&key) {
                    self.remove(&key)?;
                    self.notice(MediaNotice::Stopped { key, result })?;
                }
            }
            result = self.recordings.workers.join_next(), if !self.recordings.workers.is_empty() => {
                result.ok_or(MediaError::InvalidRoute)?.map_err(|error| MediaError::Transport(error.to_string()))?;
            }
            scheduled = frames.interval.tick(), if timed => {
                late = scheduled.elapsed() >= FRAME_PERIOD;
                self.tick()?;
            }
        }
        self.activity
            .turn(late, self.dropped.saturating_sub(before));
        Ok(Turn::Continue)
    }

    async fn close(&mut self) -> Result<(), MediaError> {
        self.playbacks.close();
        for (_, port) in std::mem::take(&mut self.ports) {
            if let Io::Task(io) = port.io {
                io.stop.send_replace(true);
            }
        }
        for (key, leg) in std::mem::take(&mut self.legs) {
            let stats = leg.finish();
            tracing::debug!(channel = %key.id, generation = key.generation, ?stats, "media leg closed");
        }
        for (key, leg) in std::mem::take(&mut self.webrtc_legs) {
            let stats = leg.finish();
            tracing::debug!(channel = %key.id, generation = key.generation, ?stats, "WebRTC leg closed");
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
