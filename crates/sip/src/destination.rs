use crate::{Engine, SipError};
use rsipstack::sip::{
    uri::{Host, Param},
    Scheme, Transport, Uri,
};
use rusterisk_config::Protocol;
use std::net::{IpAddr, SocketAddr};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetHost {
    Address(IpAddr),
    Name(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticTarget {
    host: TargetHost,
    port: Option<u16>,
    protocol: Protocol,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination {
    Registered(SocketAddr),
    Static(StaticTarget),
}

impl StaticTarget {
    pub fn from_uri(protocol: Protocol, source: &str) -> Result<Self, SipError> {
        if source.len() > 2048 {
            return Err(SipError::Capacity);
        }
        let uri = Uri::try_from(source)?;
        let secure = match &uri.scheme {
            Some(Scheme::Sip) => false,
            Some(Scheme::Sips) => true,
            Some(Scheme::Other(_)) | None => return Err(SipError::Unsupported),
        };
        if matches!(protocol, Protocol::Ws | Protocol::Wss)
            || secure && protocol != Protocol::Tls
            || !uri.headers.is_empty()
            || uri
                .auth
                .as_ref()
                .is_some_and(|auth| auth.password.is_some())
            || uri.params.iter().any(|parameter| {
                matches!(
                    parameter,
                    Param::Maddr(_) | Param::Ttl(_) | Param::Method(_)
                )
            })
        {
            return Err(SipError::Unsupported);
        }
        for parameter in &uri.params {
            if let Param::Transport(selected) = parameter {
                let matches =
                    matches!(
                        (protocol, selected),
                        (Protocol::Udp, Transport::Udp)
                            | (Protocol::Tcp, Transport::Tcp)
                            | (Protocol::Tls, Transport::Tls)
                    ) || secure && protocol == Protocol::Tls && *selected == Transport::Tcp;
                if !matches {
                    return Err(SipError::Unsupported);
                }
            }
        }
        let port = uri.host_with_port.port.map(|port| port.0);
        if port == Some(0) {
            return Err(SipError::InvalidPacket);
        }
        let host = match uri.host_with_port.host {
            Host::IpAddr(address) => {
                if address.is_unspecified() || address.is_multicast() {
                    return Err(SipError::InvalidPacket);
                }
                TargetHost::Address(address)
            }
            Host::Domain(name) => {
                if name.0.is_empty() || name.0 == "." || name.0.len() > 1024 {
                    return Err(SipError::InvalidPacket);
                }
                TargetHost::Name(name.0)
            }
        };
        Ok(Self {
            host,
            port,
            protocol,
        })
    }

    pub fn host(&self) -> &TargetHost {
        &self.host
    }

    pub const fn port(&self) -> Option<u16> {
        self.port
    }

    pub const fn protocol(&self) -> Protocol {
        self.protocol
    }
}

impl Engine {
    pub fn destination(&self, endpoint: &str, now: u64) -> Result<Destination, SipError> {
        let endpoint = self
            .config
            .endpoints
            .iter()
            .find(|entry| entry.name == endpoint)
            .ok_or(SipError::InvalidPacket)?;
        if let Some(peer) = self.registered_peer(&endpoint.name, now) {
            return Ok(Destination::Registered(peer));
        }
        let transport = self
            .config
            .transports
            .iter()
            .find(|entry| entry.name == endpoint.transport)
            .ok_or(SipError::InvalidPacket)?;
        let contact = self
            .config
            .aors
            .iter()
            .filter(|aor| endpoint.aors.contains(&aor.name))
            .find_map(|aor| aor.contact.as_deref())
            .ok_or(SipError::InvalidPacket)?;
        StaticTarget::from_uri(transport.protocol, contact).map(Destination::Static)
    }
}
