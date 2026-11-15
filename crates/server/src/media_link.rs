use rusterisk_core::{DtmfCommand, DtmfToken, DtmfTone, PeerMediaDirection};
use rusterisk_media::Frame;
use tokio::sync::{mpsc, watch};

pub struct Channels {
    pub incoming: mpsc::Sender<Frame>,
    pub digits: mpsc::Sender<DtmfTone>,
    pub outgoing: mpsc::Receiver<Frame>,
    pub commands: mpsc::Receiver<DigitCommand>,
    pub completed: mpsc::Sender<DtmfToken>,
    pub stop: watch::Receiver<bool>,
    pub direction: watch::Receiver<PeerMediaDirection>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LinkStats {
    pub packets_received: u64,
    pub packets_sent: u64,
    pub dropped: u64,
}

pub enum DigitCommand {
    Tone(DtmfTone),
    Sequence(DtmfCommand),
}
