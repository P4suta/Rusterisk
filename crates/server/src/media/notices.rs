use super::{CallKey, DtmfToken, DtmfTone, LinkStats, MediaNotice};
use rusterisk_media::MediaError;
use std::collections::VecDeque;
use tokio::sync::mpsc;

pub(super) fn service_queue_error<T>(error: mpsc::error::TrySendError<T>) -> MediaError {
    match error {
        mpsc::error::TrySendError::Full(_) => MediaError::Capacity,
        mpsc::error::TrySendError::Closed(_) => MediaError::ServiceClosed,
    }
}

const ORDINARY_CAPACITY: usize = 128;
const COMPLETION_CAPACITY: usize = 128;
// Each of the 64 admitted legs can report its first audio frame and its termination.
const LEG_CAPACITY: usize = 64 * 2;

// Reading a notice releases the slot reserved for its kind.
type QueuedNotice = (MediaNotice, mpsc::OwnedPermit<()>);

pub struct MediaNotices {
    events: mpsc::Receiver<QueuedNotice>,
    _ordinary: mpsc::Receiver<()>,
    _completions: mpsc::Receiver<()>,
    _legs: mpsc::Receiver<()>,
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
    legs: mpsc::Sender<()>,
}

pub(super) struct LegNotices {
    connected: Option<mpsc::OwnedPermit<()>>,
    stopped: mpsc::OwnedPermit<()>,
    received: VecDeque<DtmfTone>,
    pub(super) digits: Vec<(DtmfToken, mpsc::OwnedPermit<()>)>,
}

impl LegNotices {
    pub(super) fn received_digit(
        &mut self,
        sender: &NoticeSender,
        key: &CallKey,
        tone: DtmfTone,
    ) -> Result<(), MediaError> {
        if self.received.is_empty() {
            if let Some(slot) = sender.ordinary_slot()? {
                return sender.send_reserved(
                    slot,
                    MediaNotice::Dtmf {
                        key: key.clone(),
                        tone,
                    },
                );
            }
        }
        if self.received.len() >= 16 {
            return Err(MediaError::Capacity);
        }
        self.received.push_back(tone);
        Ok(())
    }

    pub(super) fn flush_received(
        &mut self,
        sender: &NoticeSender,
        key: &CallKey,
    ) -> Result<(), MediaError> {
        while let Some(&tone) = self.received.front() {
            let Some(slot) = sender.ordinary_slot()? else {
                break;
            };
            sender.send_reserved(
                slot,
                MediaNotice::Dtmf {
                    key: key.clone(),
                    tone,
                },
            )?;
            self.received.pop_front();
        }
        Ok(())
    }

    pub(super) fn connected(
        &mut self,
        sender: &NoticeSender,
        key: &CallKey,
    ) -> Result<(), MediaError> {
        if let Some(slot) = self.connected.take() {
            sender.send_reserved(slot, MediaNotice::Connected { key: key.clone() })?;
        }
        Ok(())
    }

    pub(super) fn digit_finished(
        &mut self,
        sender: &NoticeSender,
        key: &CallKey,
        token: DtmfToken,
    ) -> Result<(), MediaError> {
        if let Some(index) = self
            .digits
            .iter()
            .position(|(pending, _)| *pending == token)
        {
            let (_, slot) = self.digits.swap_remove(index);
            sender.send_reserved(
                slot,
                MediaNotice::DtmfFinished {
                    key: key.clone(),
                    token,
                },
            )?;
        }
        Ok(())
    }

    pub(super) fn stopped(
        self,
        sender: &NoticeSender,
        key: CallKey,
        result: Result<LinkStats, MediaError>,
    ) -> Result<(), MediaError> {
        sender.send_reserved(self.stopped, MediaNotice::Stopped { key, result })
    }
}

impl NoticeSender {
    pub(super) fn ordinary_slot(&self) -> Result<Option<mpsc::OwnedPermit<()>>, MediaError> {
        match self.ordinary.clone().try_reserve_owned() {
            Ok(slot) => Ok(Some(slot)),
            Err(mpsc::error::TrySendError::Full(_)) => Ok(None),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(MediaError::ServiceClosed),
        }
    }

    pub(super) fn reserve_leg(&self) -> Result<LegNotices, MediaError> {
        let reserve = || {
            self.legs
                .clone()
                .try_reserve_owned()
                .map_err(service_queue_error)
        };
        Ok(LegNotices {
            connected: Some(reserve()?),
            stopped: reserve()?,
            digits: Vec::new(),
            received: VecDeque::new(),
        })
    }

    #[cfg(test)]
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
    let (legs, leg_slots) = mpsc::channel(LEG_CAPACITY);
    let (sender, events) = mpsc::channel(ORDINARY_CAPACITY + COMPLETION_CAPACITY + LEG_CAPACITY);
    (
        NoticeSender {
            events: sender,
            ordinary,
            completions,
            legs,
        },
        MediaNotices {
            events,
            _ordinary: ordinary_slots,
            _completions: completion_slots,
            _legs: leg_slots,
        },
    )
}
