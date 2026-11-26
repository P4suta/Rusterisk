use super::stream_frames::{with_stream_port, FramePair, TaskFrames};
use super::{
    AudioSocketLink, Connection, DigitSupport, Gate, Io, MediaError, MediaHub, PeerMediaDirection,
    Port, RtpChannels, TaskIo,
};
use crate::media_websocket::{MediaLinkOptions, MediaSocket, WebSocketMediaLink};
use crate::CallKey;
use tokio::sync::{mpsc, watch};

pub(super) type ClientSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub(super) struct WebSocketAttachment<S> {
    key: CallKey,
    socket: S,
    options: MediaLinkOptions,
    lease: Option<crate::MediaConnectionLease>,
    reply: tokio::sync::oneshot::Sender<Result<(), MediaError>>,
}

impl Port {
    #[cfg(test)]
    pub(super) fn new(
        initial_direction: PeerMediaDirection,
        digit_support: DigitSupport,
    ) -> (Self, RtpChannels) {
        Self::with_frames(initial_direction, digit_support, TaskFrames::Hz48000)
    }

    fn with_frames<const N: usize>(
        initial_direction: PeerMediaDirection,
        digit_support: DigitSupport,
        pair: fn(FramePair<N>) -> TaskFrames,
    ) -> (Self, RtpChannels<N>) {
        let (incoming, input) = mpsc::channel(1);
        let (digits, digit_input) = mpsc::channel(16);
        let (outgoing, output) = mpsc::channel(1);
        let (commands, command_input) = mpsc::channel(8);
        let (completed, completions) = mpsc::channel(8);
        let (stop, stopped) = watch::channel(false);
        let (direction, directed) = watch::channel(initial_direction);
        (
            Self {
                io: Io::Task(TaskIo {
                    audio: pair(FramePair {
                        incoming: input,
                        outgoing,
                    }),
                    digits: digit_input,
                    commands,
                    completed: completions,
                    stop,
                    direction,
                }),
                ..Self::owned(initial_direction, digit_support)
            },
            RtpChannels {
                incoming,
                digits,
                outgoing: output,
                commands: command_input,
                completed,
                stop: stopped,
                direction: directed,
            },
        )
    }

    // A port for a leg that the media task runs in its own turn.
    pub(super) fn owned(
        initial_direction: PeerMediaDirection,
        digit_support: DigitSupport,
    ) -> Self {
        Self {
            connection_lease: None,
            answers: None,
            io: Io::Owned,
            digit_support,
            generated_audio_gate: match initial_direction {
                PeerMediaDirection::SendReceive => Gate::Open,
                PeerMediaDirection::SendOnly => Gate::OutgoingMuted,
                PeerMediaDirection::ReceiveOnly => Gate::IncomingMuted,
                PeerMediaDirection::Inactive => Gate::Closed,
            },
            saved_music: None,
            connection: Connection::Waiting,
        }
    }
}

impl MediaHub {
    pub(super) fn websocket_attachment<S: MediaSocket + 'static>(
        &mut self,
        attachment: WebSocketAttachment<S>,
    ) -> Result<(), MediaError> {
        let WebSocketAttachment {
            key,
            socket,
            options,
            lease,
            reply,
        } = attachment;
        if !reply.is_closed() {
            let result = self.attach_websocket(key.clone(), socket, options, lease);
            let admitted = result.is_ok();
            if reply.send(result).is_err() && admitted {
                self.remove(&key)?;
            }
        }
        Ok(())
    }

    pub(super) fn check_link(&self, key: &CallKey) -> Result<(), MediaError> {
        if self.ports.len() >= self.capacity || self.workers.len() >= self.capacity {
            return Err(MediaError::Capacity);
        }
        if self.ports.contains_key(key) || self.stream_releases.contains_key(&key.handle()) {
            return Err(MediaError::InvalidRoute);
        }
        Ok(())
    }

    pub(super) fn install_link(
        &mut self,
        key: CallKey,
        mut port: Port,
        initial_gate: Gate,
        rate: u32,
    ) -> Result<(), MediaError> {
        self.mixer.insert_at_rate(key.clone(), rate)?;
        self.mixer.gates(&key, initial_gate, initial_gate)?;
        port.generated_audio_gate = initial_gate;
        self.ports.insert(key, port);
        Ok(())
    }

    pub(super) fn attach_audiosocket(
        &mut self,
        key: CallKey,
        socket: tokio::net::TcpStream,
        rate: u32,
    ) -> Result<(), MediaError> {
        self.check_link(&key)?;
        with_stream_port!(rate, |port, channels| {
            let link = AudioSocketLink::new(socket, channels, rate)?;
            self.install_link(key.clone(), port, Gate::Open, rate)?;
            self.stream_releases
                .insert(key.handle(), super::StreamRelease::Running);
            self.workers.spawn(async move { (key, link.run().await) });
            Ok(())
        })
    }

    pub(super) fn attach_websocket<S: MediaSocket + 'static>(
        &mut self,
        key: CallKey,
        socket: S,
        options: MediaLinkOptions,
        lease: Option<crate::MediaConnectionLease>,
    ) -> Result<(), MediaError> {
        if lease
            .as_ref()
            .is_some_and(crate::MediaConnectionLease::is_released)
        {
            return Err(MediaError::UnknownStream);
        }
        self.check_link(&key)?;
        if options.channel_id != key.id {
            return Err(MediaError::InvalidRoute);
        }
        let rate = options.format.rate();
        with_stream_port!(rate, |port, channels| {
            let mut port = port;
            let (answer, answers) = mpsc::channel(8);
            port.answers = Some(answers);
            port.connection_lease = lease;
            let link = WebSocketMediaLink::new(socket, channels, options, answer)?;
            self.install_link(key.clone(), port, Gate::Open, rate)?;
            self.stream_releases
                .insert(key.handle(), super::StreamRelease::Running);
            self.workers.spawn(async move { (key, link.run().await) });
            Ok(())
        })
    }
}

impl super::MediaPort {
    pub async fn attach_websocket(
        &self,
        key: CallKey,
        socket: axum::extract::ws::WebSocket,
        options: MediaLinkOptions,
    ) -> Result<(), MediaError> {
        self.attach_media_socket(&self.websocket, key, socket, options, None)
            .await
    }

    pub async fn attach_client_websocket(
        &self,
        key: CallKey,
        socket: ClientSocket,
        options: MediaLinkOptions,
    ) -> Result<(), MediaError> {
        self.attach_media_socket(&self.client_websocket, key, socket, options, None)
            .await
    }

    pub async fn attach_claimed_websocket(
        &self,
        claimed: crate::ClaimedMediaConnection,
        socket: axum::extract::ws::WebSocket,
    ) -> Result<(), MediaError> {
        self.attach_media_socket(
            &self.websocket,
            claimed.key,
            socket,
            claimed.options,
            Some(claimed.lease),
        )
        .await
    }

    async fn attach_media_socket<S: Send>(
        &self,
        sender: &mpsc::Sender<WebSocketAttachment<S>>,
        key: CallKey,
        socket: S,
        options: MediaLinkOptions,
        lease: Option<crate::MediaConnectionLease>,
    ) -> Result<(), MediaError> {
        let (reply, response) = tokio::sync::oneshot::channel();
        sender
            .try_send(WebSocketAttachment {
                key,
                socket,
                options,
                lease,
                reply,
            })
            .map_err(|_| MediaError::Capacity)?;
        tokio::time::timeout(std::time::Duration::from_secs(5), response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))?
    }
}
