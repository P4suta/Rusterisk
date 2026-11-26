use super::{failure, media::PreparedAnswer, DigitDelivery, Signaling};
use crate::media::AttachmentRelease;
use crate::{CallKey, ControlError, Effect, Milestone, Signal};
use rusterisk_core::{MediaTaskToken, PeerMediaDirection, Wake};
use rusterisk_media::MediaError;
use rusterisk_sip::SipEvent;
use std::{collections::VecDeque, net::SocketAddr};

// A call waits this long for the media task, as its turn waited before.
const COMPLETION_MS: u64 = 5000;
// The events and effects that one call holds during one wait.
const HELD: usize = 16;

pub(super) enum Held {
    Event { listener: usize, event: SipEvent },
    Effect(Effect),
}

// What a call does when the media task answers.
pub(super) enum Waiting {
    Answered {
        release: AttachmentRelease,
        direction: PeerMediaDirection,
        events: Option<u16>,
    },
    Early {
        release: AttachmentRelease,
        listener: usize,
        token: MediaTaskToken,
        answer: PreparedAnswer,
    },
    External {
        release: AttachmentRelease,
        local: SocketAddr,
    },
    Direction {
        direction: PeerMediaDirection,
    },
}

// An answer carries the number of its command, so an answer to an expired wait completes no later wait.
pub(super) enum Outcome {
    Attached(u64, Result<(), MediaError>),
    Direction(u64, Result<bool, MediaError>),
    Expired,
}

// A call that waits for the media task handles nothing else, as its turn handled nothing else before.
pub(super) struct Pending {
    serial: u64,
    deadline: u64,
    waiting: Waiting,
    held: VecDeque<Held>,
    overflowed: bool,
}

impl Pending {
    // Whether an answer belongs to the command that this call waits for.
    fn expects(&self, outcome: &Outcome) -> bool {
        match (&self.waiting, outcome) {
            (
                Waiting::Answered { .. } | Waiting::Early { .. } | Waiting::External { .. },
                Outcome::Attached(serial, _),
            )
            | (Waiting::Direction { .. }, Outcome::Direction(serial, _)) => *serial == self.serial,
            (
                Waiting::Answered { .. }
                | Waiting::Early { .. }
                | Waiting::External { .. }
                | Waiting::Direction { .. },
                Outcome::Expired,
            ) => true,
            (
                Waiting::Answered { .. } | Waiting::Early { .. } | Waiting::External { .. },
                Outcome::Direction(..),
            )
            | (Waiting::Direction { .. }, Outcome::Attached(..)) => false,
        }
    }
}

fn late() -> MediaError {
    MediaError::Transport("media task did not answer in time".into())
}

impl Signaling {
    // Numbers the next command whose answer arrives as a notice.
    pub(super) fn next_serial(&mut self) -> u64 {
        self.media_serial = self.media_serial.wrapping_add(1);
        self.media_serial
    }

    pub(super) fn pend(&mut self, key: CallKey, serial: u64, waiting: Waiting, now: u64) {
        self.pending.insert(
            key,
            Pending {
                serial,
                deadline: now.saturating_add(COMPLETION_MS),
                waiting,
                held: VecDeque::new(),
                overflowed: false,
            },
        );
    }

    pub(super) fn waits(&self, key: &CallKey) -> bool {
        self.pending.contains_key(key)
    }

    #[cfg(test)]
    pub(super) fn waiting_serial(&self, key: &CallKey) -> Option<u64> {
        self.pending.get(key).map(|pending| pending.serial)
    }

    pub(super) fn external_waits(&self) -> usize {
        self.pending
            .values()
            .filter(|pending| matches!(pending.waiting, Waiting::External { .. }))
            .count()
    }

    pub(super) fn pending_wake(&self) -> Wake {
        self.pending.values().fold(Wake::Idle, |wake, pending| {
            wake.earliest(Wake::At(pending.deadline))
        })
    }

    // Keeps an item for a call that waits and returns every other item to its caller.
    pub(super) async fn hold(
        &mut self,
        key: Option<CallKey>,
        item: Held,
    ) -> Result<Option<Held>, ControlError> {
        let Some(key) = key else {
            return Ok(Some(item));
        };
        let Some(pending) = self.pending.get_mut(&key) else {
            return Ok(Some(item));
        };
        if pending.held.len() < HELD {
            pending.held.push_back(item);
            return Ok(None);
        }
        // A call that outgrows its queue ends, and its release follows the held items.
        if !pending.overflowed {
            pending.overflowed = true;
            tracing::warn!(channel = %key.id.as_str(), generation = key.generation, "call held too much during a media wait");
            self.notify(Signal::Terminated { key, cause: 42 }).await?;
        }
        Ok(None)
    }

    // Puts an effect that reached a waiting call back in front of the items that arrived after it.
    pub(super) fn hold_first(&mut self, key: &CallKey, effect: Effect) -> Option<Effect> {
        match self.pending.get_mut(key) {
            Some(pending) => {
                pending.held.push_front(Held::Effect(effect));
                None
            }
            None => Some(effect),
        }
    }

    pub(super) async fn expire_pending(&mut self, now: u64) -> Result<(), ControlError> {
        let expired: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.deadline <= now)
            .map(|(key, _)| key.clone())
            .collect();
        for key in expired {
            self.completed(key, Outcome::Expired, now).await?;
        }
        Ok(())
    }

    pub(super) async fn completed(
        &mut self,
        key: CallKey,
        outcome: Outcome,
        now: u64,
    ) -> Result<(), ControlError> {
        let expected = self
            .pending
            .get(&key)
            .is_some_and(|pending| pending.expects(&outcome));
        // An answer that no wait expects belongs to an expired wait, which released its media then.
        let Some(pending) = (if expected {
            self.pending.remove(&key)
        } else {
            None
        }) else {
            return Ok(());
        };
        match pending.waiting {
            Waiting::Answered {
                direction,
                events,
                release,
            } => {
                let attached = Self::attachment(&key, outcome, release).map(drop);
                self.answered(key.clone(), direction, events, attached)
                    .await?;
            }
            Waiting::Early {
                release,
                listener,
                token,
                answer,
            } => {
                let attached = Self::attachment(&key, outcome, release);
                let result = self
                    .early_attached(&key, listener, answer, attached, now)
                    .await;
                if let Err(error) = &result {
                    tracing::warn!(channel = %key.id, generation = key.generation, ?token, %error, "early media preparation failed");
                }
                self.notify(Signal::EarlyMediaPrepared {
                    key: key.clone(),
                    token,
                    result,
                })
                .await?;
            }
            Waiting::External { local, release } => {
                let attached = Self::attachment(&key, outcome, release).map(drop);
                self.external_attached(key.clone(), local, attached).await?;
            }
            Waiting::Direction { direction } => {
                let applied = match outcome {
                    Outcome::Direction(_, result) => result,
                    Outcome::Attached(..) | Outcome::Expired => Err(late()),
                };
                self.direction_applied(key.clone(), direction, applied)
                    .await?;
            }
        }
        self.replay(&key, pending.held, now).await?;
        if pending.overflowed {
            // A call that waits again ends after that wait.
            match self.pending.get_mut(&key) {
                Some(next) => next.overflowed = true,
                None => self.apply_effect(Effect::Release { key }, now).await?,
            }
        }
        Ok(())
    }

    // An attachment that arrives after its deadline finds this release behind it in the queue.
    fn attachment(
        key: &CallKey,
        outcome: Outcome,
        release: AttachmentRelease,
    ) -> Result<AttachmentRelease, MediaError> {
        match outcome {
            Outcome::Attached(_, result) => result.map(|()| release),
            Outcome::Direction(..) | Outcome::Expired => {
                release.send(key.clone());
                Err(late())
            }
        }
    }

    // Handles held items in arrival order until the call waits again.
    async fn replay(
        &mut self,
        key: &CallKey,
        mut held: VecDeque<Held>,
        now: u64,
    ) -> Result<(), ControlError> {
        while let Some(item) = held.pop_front() {
            match item {
                Held::Event { listener, event } => self.apply_event(listener, event, now).await?,
                Held::Effect(effect) => self.apply_effect(effect, now).await?,
            }
            if let Some(pending) = self.pending.get_mut(key) {
                while let Some(item) = held.pop_back() {
                    pending.held.push_front(item);
                }
            }
        }
        Ok(())
    }

    // Applies the answers of the media task until no call waits, as the serve loop does.
    #[cfg(test)]
    pub(crate) async fn media_answers_for_test(&mut self, now: u64) -> Result<(), ControlError> {
        use crate::media::MediaNotice;
        while !self.pending.is_empty() {
            let notice = self.notices.recv().await.ok_or(ControlError::Capacity)?;
            match notice {
                MediaNotice::Attached {
                    key,
                    serial,
                    result,
                } => {
                    self.completed(key, Outcome::Attached(serial, result), now)
                        .await?;
                }
                MediaNotice::DirectionApplied {
                    key,
                    serial,
                    result,
                } => {
                    self.completed(key, Outcome::Direction(serial, result), now)
                        .await?;
                }
                MediaNotice::Answer { .. }
                | MediaNotice::DtmfFinished { .. }
                | MediaNotice::Dtmf { .. }
                | MediaNotice::Connected { .. }
                | MediaNotice::Stopped { .. } => {}
            }
        }
        Ok(())
    }

    pub(super) async fn answered(
        &mut self,
        key: CallKey,
        direction: PeerMediaDirection,
        events: Option<u16>,
        attached: Result<(), MediaError>,
    ) -> Result<(), ControlError> {
        match attached {
            Ok(()) => {
                self.notify(Signal::PeerDirection {
                    key: key.clone(),
                    direction,
                })
                .await?;
                let events = match &self.calls.get(&key).ok_or(ControlError::Capacity)?.digits {
                    DigitDelivery::Signaling(_) => Some(u16::MAX),
                    DigitDelivery::Media => events,
                };
                if let Some(events) = events {
                    self.notify(Signal::DtmfReady {
                        key: key.clone(),
                        events,
                    })
                    .await?;
                }
                self.notify(Signal::Milestone {
                    key,
                    milestone: Milestone::Answered,
                })
                .await
            }
            Err(error) => {
                tracing::warn!(channel = %key.id.as_str(), generation = key.generation, %error, "media admission failed");
                self.notify(Signal::Terminated { key, cause: 21 }).await
            }
        }
    }

    pub(super) async fn external_attached(
        &mut self,
        key: CallKey,
        local: SocketAddr,
        attached: Result<(), MediaError>,
    ) -> Result<(), ControlError> {
        let result = match attached {
            Ok(()) => {
                self.external_media.insert(key.clone());
                Ok(local)
            }
            Err(error) => Err(match error {
                MediaError::Capacity => ControlError::Capacity,
                error @ (MediaError::InvalidFrame
                | MediaError::InvalidPacket
                | MediaError::InvalidRoute
                | MediaError::UnknownStream
                | MediaError::Unsupported
                | MediaError::Codec(_)
                | MediaError::Transport(_)
                | MediaError::Control(_)) => failure(error),
            }),
        };
        let admitted = result.is_ok();
        self.notify(Signal::ExternalRtpAdmitted {
            key: key.clone(),
            result,
        })
        .await?;
        if admitted {
            self.notify(Signal::Milestone {
                key,
                milestone: Milestone::Answered,
            })
            .await?;
        }
        Ok(())
    }

    // A direction change that fails ends its call and leaves the other calls running.
    pub(super) async fn direction_applied(
        &mut self,
        key: CallKey,
        direction: PeerMediaDirection,
        applied: Result<bool, MediaError>,
    ) -> Result<(), ControlError> {
        let reply = self.calls.get_mut(&key).and_then(|call| {
            call.direction_reply
                .take_if(|reply| reply.direction == direction)
        });
        match (applied, reply) {
            (Ok(true), Some(reply)) => {
                if !self.network.send_deferred(reply.transmission).await? {
                    self.notify(Signal::Terminated { key, cause: 41 }).await?;
                }
            }
            (Ok(false), Some(_)) => self.notify(Signal::Terminated { key, cause: 21 }).await?,
            (Ok(true | false), None) => {}
            (Err(error), Some(_) | None) => {
                tracing::warn!(channel = %key.id.as_str(), generation = key.generation, %error, "media direction change failed");
                self.notify(Signal::Terminated { key, cause: 21 }).await?;
            }
        }
        Ok(())
    }
}
