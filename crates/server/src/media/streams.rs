use super::{
    AudioSocketLink, Connection, DigitSupport, Gate, MediaError, MediaHub, PeerMediaDirection,
    Port, RtpChannels,
};
use crate::CallKey;
use tokio::sync::{mpsc, watch};

impl MediaHub {
    pub(super) fn attach_audiosocket(
        &mut self,
        key: CallKey,
        socket: tokio::net::TcpStream,
        rate: u32,
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
        let (direction, directed) = watch::channel(PeerMediaDirection::SendReceive);
        let link = AudioSocketLink::new(
            socket,
            RtpChannels {
                incoming,
                digits,
                outgoing: output,
                commands: command_input,
                completed,
                stop: stopped,
                direction: directed,
            },
            rate,
        )?;
        self.mixer.insert(key.clone())?;
        self.mixer.gate(&key, Gate::Open)?;
        self.ports.insert(
            key.clone(),
            Port {
                incoming: input,
                digits: digit_input,
                outgoing,
                commands,
                completed: completions,
                digit_support: DigitSupport::AudioSocket,
                stop,
                direction,
                connection: Connection::Waiting,
            },
        );
        self.workers.spawn(async move { (key, link.run().await) });
        Ok(())
    }
}
