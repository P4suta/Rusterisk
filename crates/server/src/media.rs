use crate::rtp::{LinkStats, RtpChannels, RtpLink, RtpSession, SocketPair};
use crate::CallKey;
use rusterisk_media::{Frame, Gate, MediaError, Mixer};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

pub enum MediaNotice {
    Connected {
        key: CallKey,
    },
    Stopped {
        key: CallKey,
        result: Result<LinkStats, MediaError>,
    },
}

enum Command {
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
    outgoing: mpsc::Sender<Frame>,
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
        let (outgoing, output) = mpsc::channel(3);
        let (stop, stopped) = watch::channel(false);
        let link = RtpLink::new(
            sockets,
            session,
            RtpChannels {
                incoming,
                outgoing: output,
                stop: stopped,
            },
        )?;
        self.mixer.insert(key.clone())?;
        self.ports.insert(
            key.clone(),
            Port {
                incoming: input,
                outgoing,
                stop,
                connection: Connection::Waiting,
            },
        );
        self.workers.spawn(async move { (key, link.run().await) });
        Ok(())
    }

    fn command(&mut self, command: Command) -> Result<(), MediaError> {
        match command {
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
            for _ in 0..3 {
                match port.incoming.try_recv() {
                    Ok(frame) => match self.mixer.push(key, frame) {
                        Ok(()) => {
                            if matches!(port.connection, Connection::Waiting) {
                                port.connection = Connection::Connected;
                                self.notices
                                    .try_send(MediaNotice::Connected { key: key.clone() })
                                    .map_err(|_| MediaError::Capacity)?;
                            }
                        }
                        Err(_) => self.dropped += 1,
                    },
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        port.stop.send_replace(true);
                        break;
                    }
                }
            }
        }
        for (key, frame) in self.mixer.tick()? {
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
                _instant = tick.tick() => self.tick()?,
            }
        }
        Ok(())
    }

    async fn close(&mut self) -> Result<(), MediaError> {
        for (_, port) in std::mem::take(&mut self.ports) {
            port.stop.send_replace(true);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut failure = None;
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
