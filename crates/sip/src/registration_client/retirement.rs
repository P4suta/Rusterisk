use super::{response, RegistrationTransaction, State};
use crate::SipError;
use rsipstack::sip::{SipMessage, Version};
use rusterisk_config::Protocol;
use std::net::SocketAddr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationRetirementReceipt {
    Unmatched,
    Provisional(u16),
    Final(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationRetirementStatus {
    Waiting,
    Released,
}

struct Waiting {
    correlation: response::RegistrationCorrelation,
    peer: SocketAddr,
    protocol: Protocol,
    observed: u64,
}

enum Settlement {
    Waiting(Waiting),
    Released,
}

pub struct RegistrationRetirement {
    expires: u64,
    settlement: Settlement,
}

impl RegistrationTransaction {
    pub fn prepare_retirement(&self, now: u64) -> Result<Option<RegistrationRetirement>, SipError> {
        match self.state {
            State::Trying(_) | State::Proceeding(_) => {}
            State::Ready { .. } | State::Completed { .. } | State::Terminated => return Ok(None),
        }
        if now < self.observed {
            return Err(SipError::InvalidPacket);
        }
        if now >= self.expires {
            return Ok(None);
        }
        Ok(Some(RegistrationRetirement {
            expires: self.expires,
            settlement: Settlement::Waiting(Waiting {
                correlation: response::RegistrationCorrelation::capture(&self.packet.bytes)?,
                peer: self.packet.peer,
                protocol: self.protocol,
                observed: now,
            }),
        }))
    }
}

impl RegistrationRetirement {
    pub const fn expires_at(&self) -> u64 {
        self.expires
    }

    pub fn poll(&mut self, now: u64) -> Result<RegistrationRetirementStatus, SipError> {
        let waiting = match &mut self.settlement {
            Settlement::Waiting(waiting) => waiting,
            Settlement::Released => return Ok(RegistrationRetirementStatus::Released),
        };
        if now < waiting.observed {
            return Err(SipError::InvalidPacket);
        }
        if now >= self.expires {
            self.settlement = Settlement::Released;
            return Ok(RegistrationRetirementStatus::Released);
        }
        waiting.observed = now;
        Ok(RegistrationRetirementStatus::Waiting)
    }

    pub fn receive(
        &mut self,
        bytes: &[u8],
        peer: SocketAddr,
        now: u64,
    ) -> Result<RegistrationRetirementReceipt, SipError> {
        let waiting = match &mut self.settlement {
            Settlement::Waiting(waiting) => waiting,
            Settlement::Released => return Ok(RegistrationRetirementReceipt::Unmatched),
        };
        if now >= self.expires {
            self.settlement = Settlement::Released;
            return Ok(RegistrationRetirementReceipt::Unmatched);
        }
        if peer != waiting.peer {
            return Ok(RegistrationRetirementReceipt::Unmatched);
        }
        if now < waiting.observed {
            return Err(SipError::InvalidPacket);
        }
        let response = match response::parse(bytes, waiting.protocol)? {
            SipMessage::Response(response) => response,
            SipMessage::Request(_) => return Ok(RegistrationRetirementReceipt::Unmatched),
        };
        if !waiting.correlation.matches(&response)? {
            return Ok(RegistrationRetirementReceipt::Unmatched);
        }
        let status = response.status_code.code();
        if !(100..=699).contains(&status) || response.version != Version::V2 {
            return Err(SipError::InvalidPacket);
        }
        if status < 200 {
            waiting.observed = now;
            return Ok(RegistrationRetirementReceipt::Provisional(status));
        }
        self.settlement = Settlement::Released;
        Ok(RegistrationRetirementReceipt::Final(status))
    }
}
