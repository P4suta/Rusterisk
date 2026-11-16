use super::{dialog::Dialog, notify::Provisional, ReferKey, ReferSubscription};
use crate::{RequestIdentity, SipEvent};
use rsipstack::sip::Request;
use std::collections::BTreeMap;
use std::net::SocketAddr;

pub(crate) const CAPACITY: usize = 64;
pub(crate) const LIFETIME: u64 = 600_000;
pub(crate) const TRANSACTION: u64 = 32_000;

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

    pub fn holds(&self, id: &str, generation: u64) -> bool {
        self.pending
            .values()
            .any(|owner| owner.dialog.id == id && owner.dialog.generation == generation)
            || self
                .subscriptions
                .values()
                .any(|owner| owner.dialog.id == id && owner.dialog.generation == generation)
    }

    pub fn disconnected(&mut self, peer: SocketAddr, events: &mut Vec<SipEvent>) {
        self.pending.retain(|key, owner| {
            if owner.dialog.peer != peer {
                return true;
            }
            events.push(SipEvent::ReferSubscriptionEnded { refer: *key });
            false
        });
        self.subscriptions.retain(|key, owner| {
            if owner.dialog.peer != peer {
                return true;
            }
            events.push(SipEvent::ReferSubscriptionEnded { refer: *key });
            false
        });
    }
}
