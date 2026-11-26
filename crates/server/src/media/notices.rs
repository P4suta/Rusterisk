use super::MediaNotice;
use rusterisk_media::MediaError;
use tokio::sync::mpsc;

pub(super) fn service_queue_error<T>(error: mpsc::error::TrySendError<T>) -> MediaError {
    match error {
        mpsc::error::TrySendError::Full(_) => MediaError::Capacity,
        mpsc::error::TrySendError::Closed(_) => MediaError::ServiceClosed,
    }
}

const ORDINARY_CAPACITY: usize = 128;
const COMPLETION_CAPACITY: usize = 128;

// Reading a notice releases the slot reserved for its kind.
type QueuedNotice = (MediaNotice, mpsc::OwnedPermit<()>);

pub struct MediaNotices {
    events: mpsc::Receiver<QueuedNotice>,
    _ordinary: mpsc::Receiver<()>,
    _completions: mpsc::Receiver<()>,
}

impl MediaNotices {
    pub async fn recv(&mut self) -> Option<MediaNotice> {
        self.events.recv().await.map(|(notice, _)| notice)
    }

    pub fn try_recv(&mut self) -> Result<MediaNotice, mpsc::error::TryRecvError> {
        self.events.try_recv().map(|(notice, _)| notice)
    }
}

pub(super) struct NoticeSender {
    events: mpsc::Sender<QueuedNotice>,
    pub(super) ordinary: mpsc::Sender<()>,
    pub(super) completions: mpsc::Sender<()>,
}

impl NoticeSender {
    pub(super) fn try_send(&self, notice: MediaNotice) -> Result<(), MediaError> {
        let slot = self
            .ordinary
            .clone()
            .try_reserve_owned()
            .map_err(service_queue_error)?;
        self.send_reserved(slot, notice)
    }

    pub(super) fn send_reserved(
        &self,
        slot: mpsc::OwnedPermit<()>,
        notice: MediaNotice,
    ) -> Result<(), MediaError> {
        self.events
            .try_send((notice, slot))
            .map_err(service_queue_error)
    }
}

pub(super) fn channel() -> (NoticeSender, MediaNotices) {
    let (ordinary, ordinary_slots) = mpsc::channel(ORDINARY_CAPACITY);
    let (completions, completion_slots) = mpsc::channel(COMPLETION_CAPACITY);
    let (sender, events) = mpsc::channel(ORDINARY_CAPACITY + COMPLETION_CAPACITY);
    (
        NoticeSender {
            events: sender,
            ordinary,
            completions,
        },
        MediaNotices {
            events,
            _ordinary: ordinary_slots,
            _completions: completion_slots,
        },
    )
}
