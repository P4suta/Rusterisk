use super::{
    AudioSocketLink, Connection, DigitSupport, Gate, MediaError, MediaHub, PeerMediaDirection,
    Port, RtpChannels,
};
use crate::CallKey;
use tokio::sync::{mpsc, watch};

impl Port {
    pub(super) fn new(
        initial_direction: PeerMediaDirection,
        digit_support: DigitSupport,
    ) -> (Self, RtpChannels) {
        let (incoming, input) = mpsc::channel(3);
        let (digits, digit_input) = mpsc::channel(16);
        let (outgoing, output) = mpsc::channel(3);
        let (commands, command_input) = mpsc::channel(8);
        let (completed, completions) = mpsc::channel(8);
        let (stop, stopped) = watch::channel(false);
        let (direction, directed) = watch::channel(initial_direction);
        (
            Self {
                incoming: input,
                digits: digit_input,
                outgoing,
                commands,
                completed: completions,
                digit_support,
                stop,
                direction,
                connection: Connection::Waiting,
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
}

impl MediaHub {
    pub(super) fn check_link(&self, key: &CallKey) -> Result<(), MediaError> {
        if self.ports.len() >= self.capacity || self.workers.len() >= self.capacity {
            return Err(MediaError::Capacity);
        }
        if self.ports.contains_key(key) {
            return Err(MediaError::InvalidRoute);
        }
        Ok(())
    }

    pub(super) fn install_link(
        &mut self,
        key: CallKey,
        port: Port,
        initial_direction: PeerMediaDirection,
    ) -> Result<(), MediaError> {
        self.mixer.insert(key.clone())?;
        self.mixer.gate(
            &key,
            match initial_direction {
                PeerMediaDirection::SendReceive => Gate::Open,
                PeerMediaDirection::SendOnly => Gate::OutgoingMuted,
                PeerMediaDirection::ReceiveOnly => Gate::IncomingMuted,
                PeerMediaDirection::Inactive => Gate::Closed,
            },
        )?;
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
        let (port, channels) =
            Port::new(PeerMediaDirection::SendReceive, DigitSupport::AudioSocket);
        let link = AudioSocketLink::new(socket, channels, rate)?;
        self.install_link(key.clone(), port, PeerMediaDirection::SendReceive)?;
        self.workers.spawn(async move { (key, link.run().await) });
        Ok(())
    }
}
