use super::{
    request::{expiration, header},
    RegistrationResponse,
};
use crate::{
    registration_client::{RegistrationIdentity, RegistrationTransaction},
    SipError,
};
use rusterisk_config::{Auth, Protocol};
use std::net::SocketAddr;

impl RegistrationResponse {
    /// Retries a checked 503 response through a different User Datagram Protocol (UDP) peer.
    /// Keeps the Request-URI, Call-ID, From tag, and Contact while renewing Digest headers.
    /// Requires a fresh transaction identity, a matching address family, and a nonzero peer port.
    pub fn retry_alternate(
        &self,
        credentials: &[Auth],
        identity: RegistrationIdentity,
        peer: SocketAddr,
        now: u64,
    ) -> Result<RegistrationTransaction, SipError> {
        if self.status() != 503 {
            return Err(SipError::Unsupported);
        }
        match self.protocol {
            Protocol::Udp => {}
            Protocol::Tcp | Protocol::Tls | Protocol::Ws | Protocol::Wss => {
                return Err(SipError::Unsupported);
            }
        }
        let _instruction = self.retry_after()?;
        if peer == self.request.peer
            || peer.port() == 0
            || peer.ip().is_unspecified()
            || peer.ip().is_multicast()
            || peer.is_ipv4() != self.request.peer.is_ipv4()
        {
            return Err(SipError::InvalidPacket);
        }
        let source = std::str::from_utf8(&self.request.bytes)?;
        let expiration = expiration(header(source, "Expires"))?.ok_or(SipError::InvalidPacket)?;
        let mut transaction = self.with_expiration(credentials, identity, expiration, now)?;
        transaction.packet.peer = peer;
        transaction.authentication.attempts = 0;
        Ok(transaction)
    }
}
