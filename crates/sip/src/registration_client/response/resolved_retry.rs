use super::RegistrationResponse;
use crate::{registration_client::RegistrationIdentity, RegistrationTransaction, SipError};
use rusterisk_config::{Auth, Protocol};
use std::net::SocketAddr;

impl RegistrationResponse {
    /// Continues a completed User Datagram Protocol (UDP) registration failure after resolution.
    /// Keeps the transmitted request identity and origin and proxy Digest state.
    /// The caller enforces peer retry deadlines and the configuration lifetime.
    pub fn retry_after_resolution(
        &self,
        credentials: &[Auth],
        identity: RegistrationIdentity,
        peer: SocketAddr,
        now: u64,
    ) -> Result<RegistrationTransaction, SipError> {
        match self.protocol {
            Protocol::Udp => {}
            Protocol::Tcp | Protocol::Tls | Protocol::Ws | Protocol::Wss => {
                return Err(SipError::Unsupported);
            }
        }
        if peer.port() == 0
            || peer.ip().is_unspecified()
            || peer.ip().is_multicast()
            || peer.is_ipv4() != self.request.peer.is_ipv4()
        {
            return Err(SipError::InvalidPacket);
        }
        let mut transaction = self.retry_after_failure(credentials, identity, now)?;
        transaction.packet.peer = peer;
        Ok(transaction)
    }
}
