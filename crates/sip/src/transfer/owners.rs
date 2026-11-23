use super::{dialog::Dialog, notify::Provisional, ReferKey, ReferSubscription};
use crate::{Disconnection, RequestIdentity, SipEvent};
use rsipstack::sip::Request;
use std::collections::BTreeMap;

pub(crate) const CAPACITY: usize = 64;
pub(crate) const LIFETIME: u64 = 600_000;
pub(crate) const TRANSACTION: u64 = 32_000;
pub(crate) const RINGING_INTERVAL: u64 = 1000;

pub(crate) struct Pending {
    pub dialog: Dialog,
    pub request: Request,
    pub identity: RequestIdentity,
    pub subscription: ReferSubscription,
    pub sequence: u32,
    pub deadline: u64,
}

pub(crate) struct Subscription {
    pub dialog: Dialog,
    pub sequence: u32,
    pub expires: u64,
    pub state: State,
}

pub(crate) enum State {
    Active {
        last: Provisional,
        sent: u64,
        pending: Option<Provisional>,
    },
    Terminal {
        deadline: u64,
        sequence: u32,
    },
}

#[derive(Default)]
pub(crate) struct Owners {
    pub pending: BTreeMap<ReferKey, Pending>,
    pub subscriptions: BTreeMap<ReferKey, Subscription>,
}

impl Owners {
    pub fn len(&self) -> usize {
        self.pending.len() + self.subscriptions.len()
    }

    pub fn wake(&self) -> rusterisk_core::Wake {
        use rusterisk_core::Wake;
        let pending = self.pending.values().fold(Wake::Idle, |wake, owner| {
            wake.earliest(Wake::At(owner.deadline))
        });
        self.subscriptions.values().fold(pending, |wake, owner| {
            wake.earliest(match owner.state {
                State::Terminal { deadline, .. } => Wake::At(deadline),
                State::Active {
                    sent,
                    pending: Some(Provisional::Ringing),
                    ..
                } => Wake::At(owner.expires)
                    .earliest(Wake::At(sent.saturating_add(RINGING_INTERVAL))),
                State::Active { .. } => Wake::At(owner.expires),
            })
        })
    }

    pub fn holds(&self, id: &str, generation: u64) -> bool {
        self.pending
            .values()
            .any(|owner| owner.dialog.id == id && owner.dialog.generation == generation)
            || self
                .subscriptions
                .values()
                .any(|owner| owner.dialog.id == id && owner.dialog.generation == generation)
    }

    pub fn disconnected(&mut self, closed: Disconnection, events: &mut Vec<SipEvent>) {
        self.pending.retain(|key, owner| {
            if !closed.matches(owner.dialog.route) {
                return true;
            }
            events.push(SipEvent::ReferSubscriptionEnded { refer: *key });
            false
        });
        self.subscriptions.retain(|key, owner| {
            if !closed.matches(owner.dialog.route) {
                return true;
            }
            events.push(SipEvent::ReferSubscriptionEnded { refer: *key });
            false
        });
    }
}
