use super::{Client, ConfigurationLifetime, Registrations, State};
use crate::{reload::PublicationKey, signaling::transport::Network, ControlError};
use rusterisk_config::Config;
use rusterisk_core::SettingsGeneration;
use std::num::NonZeroU64;

pub(in crate::signaling) struct PreparedRegistrations {
    clients: Vec<Client>,
}

pub(super) struct RegistrationReservation {
    key: PublicationKey,
    generation: NonZeroU64,
    next: NonZeroU64,
    clock_limit: u64,
    retirement: super::retiring::RetirementPlan,
}

impl PreparedRegistrations {
    pub(in crate::signaling) fn prepare(config: &Config) -> Result<Self, ControlError> {
        Ok(Self {
            clients: Registrations::clients_at(config, 0)?,
        })
    }

    fn check_clock(&self, now: u64) -> Result<(), ControlError> {
        for client in &self.clients {
            match &client.state {
                State::Waiting { at } | State::Ready { at, .. } => {
                    now.checked_add(*at).ok_or(ControlError::Capacity)?;
                }
                State::CandidateWait { .. }
                | State::Alternate { .. }
                | State::Resolving { .. }
                | State::Connecting { .. }
                | State::Trying { .. }
                | State::Registered { .. }
                | State::Backoff { .. }
                | State::Stopped => {
                    return Err(ControlError::Invalid("Registration preparation changed"));
                }
            }
        }
        Ok(())
    }

    fn activate(mut self, now: u64) -> Result<Vec<Client>, ControlError> {
        self.check_clock(now)?;
        for client in &mut self.clients {
            match &mut client.state {
                State::Waiting { at } | State::Ready { at, .. } => {
                    *at = now.checked_add(*at).ok_or(ControlError::Capacity)?;
                }
                State::CandidateWait { .. }
                | State::Alternate { .. }
                | State::Resolving { .. }
                | State::Connecting { .. }
                | State::Trying { .. }
                | State::Registered { .. }
                | State::Backoff { .. }
                | State::Stopped => {
                    return Err(ControlError::Invalid("Registration preparation changed"));
                }
            }
        }
        Ok(self.clients)
    }
}

impl Registrations {
    pub(in crate::signaling) fn quiesced(&self, key: PublicationKey) -> bool {
        self.reservation.as_ref().is_some_and(|reservation| {
            reservation.key == key && reservation.generation == self.generation()
        })
    }

    pub(in crate::signaling) fn quiesce(
        &mut self,
        key: PublicationKey,
        prepared: &PreparedRegistrations,
        network: &Network,
        now: u64,
        clock_limit: u64,
    ) -> Result<(), ControlError> {
        if let Some(reservation) = &self.reservation {
            return if reservation.key == key && reservation.generation == self.generation() {
                Ok(())
            } else {
                Err(ControlError::Capacity)
            };
        }
        if clock_limit < now {
            return Err(ControlError::Capacity);
        }
        prepared.check_clock(clock_limit)?;
        let next = self
            .generation()
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or(ControlError::Capacity)?;
        let retirement = self.retiring.prepare(&self.clients, network, now)?;
        self.reservation = Some(RegistrationReservation {
            key,
            generation: self.generation(),
            next,
            clock_limit,
            retirement,
        });
        Ok(())
    }

    pub(in crate::signaling) fn abort_publication(
        &mut self,
        key: PublicationKey,
    ) -> Result<(), ControlError> {
        if self
            .reservation
            .as_ref()
            .is_some_and(|reservation| reservation.key != key)
        {
            return Err(ControlError::Conflict("Registration publication changed"));
        }
        self.reservation = None;
        Ok(())
    }

    pub(in crate::signaling) fn publish(
        &mut self,
        key: PublicationKey,
        prepared: PreparedRegistrations,
        settings: SettingsGeneration,
        now: u64,
    ) -> Result<(), ControlError> {
        let reservation = self
            .reservation
            .as_ref()
            .ok_or(ControlError::Conflict("Registration reservation missing"))?;
        if reservation.key != key
            || reservation.generation != self.generation()
            || now > reservation.clock_limit
        {
            return Err(ControlError::Conflict("Registration publication changed"));
        }
        self.retiring.check_clock(now)?;
        let clients = prepared.activate(now)?;
        let lifetime = ConfigurationLifetime::new(reservation.next);
        let reservation = self
            .reservation
            .take()
            .ok_or(ControlError::Conflict("Registration reservation missing"))?;
        self.lifetime.cancel();
        self.retiring
            .install(reservation.retirement, &self.clients, now);
        self.clients = clients;
        self.cursor = 0;
        self.lifetime = lifetime;
        self.settings_epoch = Some(settings);
        Ok(())
    }

    pub(super) fn allows_transaction(&self, index: usize, client: &Client, now: u64) -> bool {
        self.reservation
            .as_ref()
            .is_none_or(|reservation| reservation.retirement.allows(index, client, now))
    }

    pub(super) fn transaction_allowed_from(&self, index: usize, client: &Client) -> Option<u64> {
        match &self.reservation {
            Some(reservation) => reservation.retirement.allowed_from(index, client),
            None => Some(0),
        }
    }
}
