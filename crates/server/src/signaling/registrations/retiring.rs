use super::{default_port, failure, Client, Route, State};
use crate::{
    signaling::{
        transport::{DeliveryRoute, Network},
        Received,
    },
    ControlError,
};
use rusterisk_config::MAX_REGISTRATIONS;
use rusterisk_sip::{
    RegistrationRetirement, RegistrationRetirementReceipt, RegistrationRetirementStatus,
    StaticTarget, TargetHost,
};
use std::net::SocketAddr;

pub(super) struct Retired {
    pub(super) retirement: RegistrationRetirement,
    pub(super) registrar: StaticTarget,
    pub(super) route: DeliveryRoute,
}

pub(super) struct Retiring {
    pub(super) slots: [Option<Retired>; MAX_REGISTRATIONS],
    observed: u64,
}

pub(super) struct RetirementPlan {
    originals: [Option<rusterisk_sip::RegistrationIdentity>; MAX_REGISTRATIONS],
    slots: [Option<ReservedRetirement>; MAX_REGISTRATIONS],
}

struct ReservedRetirement {
    index: usize,
    identity: rusterisk_sip::RegistrationIdentity,
    retired: Retired,
}

impl RetirementPlan {
    pub(super) fn allowed_from(&self, index: usize, client: &Client) -> Option<u64> {
        let State::Trying { transaction, .. } = &client.state else {
            return None;
        };
        if self
            .originals
            .get(index)
            .is_none_or(|identity| *identity != Some(transaction.identity()))
        {
            return None;
        }
        let reserved = self
            .slots
            .iter()
            .flatten()
            .any(|entry| entry.index == index && entry.identity == transaction.identity());
        Some(if reserved {
            0
        } else {
            transaction.expires_at()
        })
    }

    pub(super) fn allows(&self, index: usize, client: &Client, now: u64) -> bool {
        self.allowed_from(index, client)
            .is_some_and(|from| from <= now)
    }
}

pub(super) fn same_registrar(first: &StaticTarget, second: &StaticTarget) -> bool {
    let Ok(default_port) = default_port(first.protocol()) else {
        return false;
    };
    first.protocol() == second.protocol()
        && first.port().unwrap_or(default_port) == second.port().unwrap_or(default_port)
        && match (first.host(), second.host()) {
            (TargetHost::Address(first), TargetHost::Address(second)) => first == second,
            (TargetHost::Name(first), TargetHost::Name(second)) => first
                .trim_end_matches('.')
                .eq_ignore_ascii_case(second.trim_end_matches('.')),
            (TargetHost::Address(_), TargetHost::Name(_))
            | (TargetHost::Name(_), TargetHost::Address(_)) => false,
        }
}

impl Retiring {
    pub(super) fn new(now: u64) -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            observed: now,
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    pub(super) fn check_clock(&self, now: u64) -> Result<(), ControlError> {
        if now < self.observed {
            Err(ControlError::Invalid("Registration clock moved backwards"))
        } else {
            Ok(())
        }
    }

    pub(super) fn prepare(
        &self,
        clients: &[Client],
        network: &Network,
        now: u64,
    ) -> Result<RetirementPlan, ControlError> {
        self.check_clock(now)?;
        let mut plan = RetirementPlan {
            originals: [None; MAX_REGISTRATIONS],
            slots: std::array::from_fn(|_| None),
        };
        let mut vacant = self.slots.iter().enumerate().filter(|(_, slot)| {
            slot.as_ref()
                .is_none_or(|entry| entry.retirement.expires_at() <= now)
        });
        for (index, client) in clients.iter().enumerate() {
            let State::Trying { transaction, route } = &client.state else {
                continue;
            };
            *plan
                .originals
                .get_mut(index)
                .ok_or(ControlError::Capacity)? = Some(transaction.identity());
            let Some(retirement) = transaction.prepare_retirement(now).map_err(failure)? else {
                continue;
            };
            let route = match route {
                Route::Datagram(peer) => network.capture(client.listener, *peer)?,
                Route::Stream(route) => *route,
            };
            let (slot, _) = vacant.next().ok_or(ControlError::Capacity)?;
            plan.slots[slot] = Some(ReservedRetirement {
                index,
                identity: transaction.identity(),
                retired: Retired {
                    retirement,
                    registrar: client.target.clone(),
                    route,
                },
            });
        }
        Ok(plan)
    }

    pub(super) fn install(&mut self, plan: RetirementPlan, clients: &[Client], now: u64) {
        for (slot, incoming) in self.slots.iter_mut().zip(plan.slots) {
            if slot
                .as_ref()
                .is_some_and(|entry| entry.retirement.expires_at() <= now)
            {
                *slot = None;
            }
            if let Some(incoming) = incoming {
                let current = clients.get(incoming.index).is_some_and(|client| {
                    matches!(&client.state, State::Trying { transaction, .. }
                        if transaction.identity() == incoming.identity
                            && !transaction.is_finished()
                            && transaction.expires_at() > now)
                });
                if current {
                    *slot = Some(incoming.retired);
                }
            }
        }
        self.observed = now;
    }

    pub(super) fn blocked(
        &self,
        target: &StaticTarget,
        peer: Option<SocketAddr>,
        now: u64,
    ) -> bool {
        self.slots.iter().flatten().any(|entry| {
            entry.retirement.expires_at() > now
                && (peer.is_some_and(|peer| peer == entry.route.peer())
                    || same_registrar(&entry.registrar, target))
        })
    }

    pub(super) fn wake(&self) -> rusterisk_core::Wake {
        use rusterisk_core::Wake;
        self.slots.iter().flatten().fold(Wake::Idle, |wake, entry| {
            wake.earliest(Wake::At(entry.retirement.expires_at()))
        })
    }

    pub(super) fn poll(&mut self, now: u64) -> Result<(), ControlError> {
        self.check_clock(now)?;
        for slot in &mut self.slots {
            if let Some(entry) = slot {
                if entry.retirement.poll(now).map_err(failure)?
                    == RegistrationRetirementStatus::Released
                {
                    *slot = None;
                }
            }
        }
        self.observed = now;
        Ok(())
    }

    fn receive_matching(
        &mut self,
        bytes: &[u8],
        peer: SocketAddr,
        matches: impl Fn(DeliveryRoute) -> bool,
        now: u64,
    ) -> Result<RegistrationRetirementReceipt, ControlError> {
        self.poll(now)?;
        for slot in &mut self.slots {
            let Some(entry) = slot else {
                continue;
            };
            if !matches(entry.route) {
                continue;
            }
            let received = entry
                .retirement
                .receive(bytes, peer, now)
                .map_err(failure)?;
            match received {
                RegistrationRetirementReceipt::Unmatched => {}
                RegistrationRetirementReceipt::Provisional(_) => return Ok(received),
                RegistrationRetirementReceipt::Final(_) => {
                    *slot = None;
                    return Ok(received);
                }
            }
        }
        Ok(RegistrationRetirementReceipt::Unmatched)
    }

    pub(super) fn receive_datagram(
        &mut self,
        packet: &Received,
        now: u64,
    ) -> Result<RegistrationRetirementReceipt, ControlError> {
        self.receive_matching(
            &packet.bytes,
            packet.peer,
            |route| route == packet.route(),
            now,
        )
    }

    pub(super) fn clear(&mut self) {
        self.slots = std::array::from_fn(|_| None);
    }
}
