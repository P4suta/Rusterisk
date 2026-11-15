use crate::rtp::{LinkStats, RtpChannels, RtpDigitCommand, RtpLink, RtpSession, SocketPair};
use crate::storage::{PlaybackSource, RecordingSink, WorkerError};
use crate::CallKey;
use rusterisk_core::{DtmfCommand, DtmfToken, DtmfTone, MediaTaskToken};
use rusterisk_media::{AudioTarget, Frame, Gate, MediaError, Mixer, TelephoneFormat};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;
mod playback;
mod recording;
pub use playback::PlaybackControl;
use playback::Playbacks;
use recording::Recordings;
pub use recording::{RecordingEnd, RecordingTarget};

pub enum MediaNotice {
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

enum Command {
    Play {
        token: MediaTaskToken,
        target: AudioTarget<CallKey>,
        source: PlaybackSource,
        completed: oneshot::Sender<Result<(), MediaError>>,
        reply: oneshot::Sender<Result<(), MediaError>>,
    },
    ControlPlayback {
        token: MediaTaskToken,
        operation: PlaybackControl,
    },
    Record {
        token: MediaTaskToken,
        target: RecordingTarget,
        sink: RecordingSink,
        completed: oneshot::Sender<Result<(), WorkerError>>,
        reply: oneshot::Sender<Result<(), MediaError>>,
    },
    EndRecording {
        token: MediaTaskToken,
        mode: RecordingEnd,
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
        reply: oneshot::Sender<Result<(), MediaError>>,
    },
    Release {
        key: CallKey,
    },
    Bridge {
        id: u64,
        members: Vec<CallKey>,
    },
    Gate {
        key: CallKey,
        gate: Gate,
    },
}

#[derive(Clone)]
pub struct MediaPort {
    commands: mpsc::Sender<Command>,
}

impl MediaPort {
    pub async fn play(
        &self,
        target: AudioTarget<CallKey>,
        token: MediaTaskToken,
        source: PlaybackSource,
    ) -> Result<oneshot::Receiver<Result<(), MediaError>>, MediaError> {
        let (completed, completion) = oneshot::channel();
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::Play {
                token,
                target,
                source,
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
        let (completed, completion) = oneshot::channel();
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::Record {
                token,
                target,
                sink,
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
    ) -> Result<(), MediaError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::Attach {
                key,
                sockets,
                session,
                reply,
            })
            .map_err(|_| MediaError::Capacity)?;
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))?
    }
    pub fn release(&self, key: CallKey) -> Result<(), MediaError> {
        self.commands
            .try_send(Command::Release { key })
            .map_err(|_| MediaError::Capacity)
    }
    pub fn bridge(&self, id: u64, members: Vec<CallKey>) -> Result<(), MediaError> {
        if members.len() > 64 {
            return Err(MediaError::Capacity);
        }
        self.commands
            .try_send(Command::Bridge { id, members })
            .map_err(|_| MediaError::Capacity)
    }
    pub fn gate(&self, key: CallKey, gate: Gate) -> Result<(), MediaError> {
        self.commands
            .try_send(Command::Gate { key, gate })
            .map_err(|_| MediaError::Capacity)
    }
}

enum Connection {
    Waiting,
    Connected,
}
struct Port {
    incoming: mpsc::Receiver<Frame>,
    digits: mpsc::Receiver<DtmfTone>,
    outgoing: mpsc::Sender<Frame>,
    commands: mpsc::Sender<RtpDigitCommand>,
    completed: mpsc::Receiver<DtmfToken>,
    telephone: Option<TelephoneFormat>,
    stop: watch::Sender<bool>,
    connection: Connection,
}

pub struct MediaHub {
    capacity: usize,
    commands: mpsc::Receiver<Command>,
    notices: mpsc::Sender<MediaNotice>,
    mixer: Mixer<CallKey>,
    ports: BTreeMap<CallKey, Port>,
    workers: JoinSet<(CallKey, Result<LinkStats, MediaError>)>,
    dropped: u64,
    recordings: Recordings,
    playbacks: Playbacks,
}

impl MediaHub {
    pub fn new(capacity: usize) -> (MediaPort, Self, mpsc::Receiver<MediaNotice>) {
        let (commands, receiver) = mpsc::channel(128);
        let (notices, events) = mpsc::channel(128);
        (
            MediaPort { commands },
            Self {
                capacity: capacity.min(64),
                commands: receiver,
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
    ) -> Result<(), MediaError> {
        if self.ports.len() >= self.capacity || self.workers.len() >= self.capacity {
            return Err(MediaError::Capacity);
        }
        if self.ports.contains_key(&key) {
            return Err(MediaError::InvalidRoute);
        }
        let (incoming, input) = mpsc::channel(3);
        let (digits, digit_input) = mpsc::channel(16);
        let (outgoing, output) = mpsc::channel(3);
        let (commands, command_input) = mpsc::channel(8);
        let (completed, completions) = mpsc::channel(8);
        let (stop, stopped) = watch::channel(false);
        let link = RtpLink::new(
            sockets,
            session,
            RtpChannels {
                incoming,
                digits,
                outgoing: output,
                commands: command_input,
                completed,
                stop: stopped,
            },
        )?;
        self.mixer.insert(key.clone())?;
        self.ports.insert(
            key.clone(),
            Port {
                incoming: input,
                digits: digit_input,
                outgoing,
                commands,
                completed: completions,
                telephone: session.telephone,
                stop,
                connection: Connection::Waiting,
            },
        );
        self.workers.spawn(async move { (key, link.run().await) });
        Ok(())
    }

    fn command(&mut self, command: Command) -> Result<(), MediaError> {
        match command {
            Command::Play {
                token,
                target,
                source,
                completed,
                reply,
            } => {
                if !reply.is_closed() {
                    let valid = match &target {
                        AudioTarget::Channel(key) => self.ports.contains_key(key),
                        AudioTarget::Bridge(_) => true,
                    };
                    let result = if valid {
                        self.playbacks.begin(token, target, source, completed)
                    } else {
                        Err(MediaError::UnknownStream)
                    };
                    let admitted = result.is_ok();
                    if reply.send(result).is_err() && admitted {
                        self.playbacks.end(token, Ok(()));
                    }
                }
            }
            Command::ControlPlayback { token, operation } => {
                self.playbacks.control(token, operation)
            }
            Command::Record {
                token,
                target,
                sink,
                completed,
                reply,
            } => {
                if !reply.is_closed() {
                    let valid = match &target {
                        RecordingTarget::Channel(key) => self.ports.contains_key(key),
                        RecordingTarget::Bridge(_) => true,
                    };
                    let result = if valid {
                        self.recordings.begin(token, target, sink, completed)
                    } else {
                        Err(MediaError::UnknownStream)
                    };
                    let admitted = result.is_ok();
                    if reply.send(result).is_err() && admitted {
                        self.recordings.end(token, RecordingEnd::Cancel);
                    }
                }
            }
            Command::EndRecording { token, mode } => self.recordings.end(token, mode),
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
                            let format = port.telephone.ok_or(MediaError::Unsupported)?;
                            let supported = match &command {
                                RtpDigitCommand::Tone(tone) => format.supports(tone.digit),
                                RtpDigitCommand::Sequence(command) => {
                                    command.sequence.supported_by(format.events())
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
                reply,
            } => {
                if !reply.is_closed() {
                    let result = self.attach(key.clone(), sockets, session);
                    if reply.send(result).is_err() {
                        self.remove(&key)?;
                    }
                }
            }
            Command::Release { key } => self.remove(&key)?,
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
            Command::Gate { key, gate } => {
                if self.ports.contains_key(&key) {
                    self.mixer.gate(&key, gate)?;
                }
            }
        }
        Ok(())
    }

    fn tick(&mut self) -> Result<(), MediaError> {
        for (key, port) in &mut self.ports {
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
                    Ok(tone) => self
                        .notices
                        .try_send(MediaNotice::Dtmf {
                            key: key.clone(),
                            tone,
                        })
                        .map_err(|_| MediaError::Capacity)?,
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
        let playback = self.playbacks.tick()?;
        let audio = self.mixer.tick_with_playback(&playback)?;
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
