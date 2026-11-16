use crate::{Datagram, SipError, MAX_MESSAGE};
use rusterisk_config::{Protocol, Registration};
use std::net::SocketAddr;
mod response;
pub use response::{RegistrationAuthentication, RegistrationGrant, RegistrationResponse};

const TIMEOUT: u64 = 32_000;
const INITIAL_RETRY: u64 = 500;
const MAX_RETRY: u64 = 4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistrationIdentity {
    boot: u128,
    sequence: u32,
    branch: u128,
}

impl RegistrationIdentity {
    pub fn new(boot: u128, sequence: u32, branch: u128) -> Result<Self, SipError> {
        if sequence >= 1 << 31 {
            return Err(SipError::Capacity);
        }
        Ok(Self {
            boot,
            sequence,
            branch,
        })
    }

    pub fn advance(self, branch: u128) -> Result<Self, SipError> {
        let sequence = self.sequence.checked_add(1).ok_or(SipError::Capacity)?;
        Self::new(self.boot, sequence, branch)
    }
}

pub enum RegistrationTransactionEvent {
    Transmit(Datagram),
    Provisional(u16),
    Response(RegistrationResponse),
    TimedOut,
}

#[derive(Clone, Copy)]
enum Retransmission {
    Reliable,
    Datagram { at: u64, interval: u64 },
}

#[derive(Clone, Copy)]
enum State {
    Ready { at: u64 },
    Trying(Retransmission),
    Proceeding(Retransmission),
    Completed { until: u64 },
    Terminated,
}

pub struct RegistrationTransaction {
    identity: RegistrationIdentity,
    packet: Datagram,
    protocol: Protocol,
    expires: u64,
    observed: u64,
    state: State,
    authentication_attempts: u8,
}

impl RegistrationTransaction {
    pub fn new(
        settings: &Registration,
        local: SocketAddr,
        peer: SocketAddr,
        identity: RegistrationIdentity,
        now: u64,
    ) -> Result<Self, SipError> {
        let expires = now.checked_add(TIMEOUT).ok_or(SipError::Capacity)?;
        let packet = request(settings, local, peer, identity)?;
        Ok(Self {
            identity,
            packet,
            protocol: settings.server_uri.protocol(),
            expires,
            observed: now,
            state: State::Ready { at: now },
            authentication_attempts: 0,
        })
    }

    pub const fn identity(&self) -> RegistrationIdentity {
        self.identity
    }

    pub const fn expires_at(&self) -> u64 {
        self.expires
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.state, State::Terminated)
    }

    pub fn next_deadline(&self) -> Option<u64> {
        match self.state {
            State::Ready { at } => Some(at),
            State::Trying(Retransmission::Reliable)
            | State::Proceeding(Retransmission::Reliable) => Some(self.expires),
            State::Trying(Retransmission::Datagram { at, .. })
            | State::Proceeding(Retransmission::Datagram { at, .. }) => Some(at.min(self.expires)),
            State::Completed { until } => Some(until),
            State::Terminated => None,
        }
    }

    pub fn cancel(&mut self) {
        self.state = State::Terminated;
        self.packet.bytes.clear();
    }

    pub fn poll(&mut self, now: u64) -> Result<Option<RegistrationTransactionEvent>, SipError> {
        if self.is_finished() {
            return Ok(None);
        }
        if now < self.observed {
            return Err(SipError::InvalidPacket);
        }
        self.observed = now;
        if let State::Completed { until } = self.state {
            if now >= until {
                self.cancel();
            }
            return Ok(None);
        }
        if now >= self.expires {
            self.cancel();
            return Ok(Some(RegistrationTransactionEvent::TimedOut));
        }
        self.state = match self.state {
            State::Ready { .. } => State::Trying(match self.protocol {
                Protocol::Udp => Retransmission::Datagram {
                    at: self.retry_at(now, INITIAL_RETRY),
                    interval: INITIAL_RETRY,
                },
                Protocol::Tcp | Protocol::Tls => Retransmission::Reliable,
                Protocol::Ws | Protocol::Wss => return Err(SipError::Unsupported),
            }),
            State::Trying(Retransmission::Datagram { at, interval }) if now >= at => {
                let interval = (interval * 2).min(MAX_RETRY);
                State::Trying(Retransmission::Datagram {
                    at: self.retry_at(now, interval),
                    interval,
                })
            }
            State::Proceeding(Retransmission::Datagram { at, .. }) if now >= at => {
                State::Proceeding(Retransmission::Datagram {
                    at: self.retry_at(now, MAX_RETRY),
                    interval: MAX_RETRY,
                })
            }
            State::Trying(Retransmission::Reliable | Retransmission::Datagram { .. })
            | State::Proceeding(Retransmission::Reliable | Retransmission::Datagram { .. })
            | State::Completed { .. }
            | State::Terminated => return Ok(None),
        };
        Ok(Some(RegistrationTransactionEvent::Transmit(Datagram::new(
            self.packet.peer,
            self.packet.bytes.clone(),
        ))))
    }

    fn retry_at(&self, now: u64, interval: u64) -> u64 {
        now.checked_add(interval)
            .map_or(self.expires, |at| at.min(self.expires))
    }
}

fn request(
    settings: &Registration,
    local: SocketAddr,
    peer: SocketAddr,
    identity: RegistrationIdentity,
) -> Result<Datagram, SipError> {
    for address in [local, peer] {
        if address.port() == 0 || address.ip().is_unspecified() || address.ip().is_multicast() {
            return Err(SipError::InvalidPacket);
        }
    }
    let protocol = settings.server_uri.protocol();
    if settings.client_uri.is_secure() && protocol != Protocol::Tls {
        return Err(SipError::Unsupported);
    }
    if settings.server_uri.is_secure() != settings.client_uri.is_secure() {
        return Err(SipError::Unsupported);
    }
    let (token, parameter, scheme) = match protocol {
        Protocol::Udp => ("UDP", "udp", "sip"),
        Protocol::Tcp => ("TCP", "tcp", "sip"),
        Protocol::Tls => (
            "TLS",
            "tcp",
            if settings.client_uri.is_secure() {
                "sips"
            } else {
                "sip"
            },
        ),
        Protocol::Ws | Protocol::Wss => return Err(SipError::Unsupported),
    };
    validate_user(&settings.contact_user)?;
    let source = format!(
        "REGISTER {} SIP/2.0\r\nVia: SIP/2.0/{token} {local};rport;branch=z9hG4bK-reg-{:032x}-{}\r\nFrom: <{}>;tag=reg-{:032x}\r\nTo: <{}>\r\nCall-ID: reg-{:032x}@rusterisk\r\nCSeq: {} REGISTER\r\nMax-Forwards: 70\r\nContact: <{scheme}:{}@{local};transport={parameter}>\r\nExpires: {}\r\nUser-Agent: Rusterisk\r\nContent-Length: 0\r\n\r\n",
        settings.server_uri.as_str(),
        identity.branch,
        identity.sequence,
        settings.client_uri.as_str(),
        identity.branch,
        settings.client_uri.as_str(),
        identity.boot,
        identity.sequence,
        settings.contact_user,
        settings.expiration,
    );
    if source.len() > MAX_MESSAGE {
        return Err(SipError::Capacity);
    }
    Ok(Datagram::new(peer, source.into_bytes()))
}

fn validate_user(source: &str) -> Result<(), SipError> {
    if source.len() > 256 {
        return Err(SipError::Capacity);
    }
    if source.is_empty() {
        return Err(SipError::InvalidPacket);
    }
    let mut bytes = source.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            if !bytes.next().is_some_and(|byte| byte.is_ascii_hexdigit())
                || !bytes.next().is_some_and(|byte| byte.is_ascii_hexdigit())
            {
                return Err(SipError::InvalidPacket);
            }
        } else if !byte.is_ascii_alphanumeric() && !b"-_.!~*'()&=+$,;?/".contains(&byte) {
            return Err(SipError::InvalidPacket);
        }
    }
    Ok(())
}
