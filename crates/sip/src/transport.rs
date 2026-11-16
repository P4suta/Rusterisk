use crate::{dialog_target::DialogTarget, ContactError, RegistrationContacts, SipError};
use rsipstack::sip::{
    uri::{Param, Scheme},
    Request, Transport, Uri,
};
use rusterisk_config::Protocol;
use std::net::SocketAddr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UriSecurity {
    Sip,
    Sips,
}

pub(super) struct IncomingTarget {
    pub(super) security: UriSecurity,
    pub(super) target: DialogTarget,
}

pub(super) enum IncomingSecurityError {
    Invalid,
    UnsupportedScheme,
    SecureTransportRequired,
    Capacity,
}

impl From<ContactError> for IncomingSecurityError {
    fn from(error: ContactError) -> Self {
        match error {
            ContactError::Invalid | ContactError::Unsupported => Self::Invalid,
            ContactError::Capacity => Self::Capacity,
        }
    }
}

impl UriSecurity {
    fn address(uri: &Uri) -> Result<Self, SipError> {
        match &uri.scheme {
            Some(Scheme::Sip) => Ok(Self::Sip),
            Some(Scheme::Sips) => Ok(Self::Sips),
            Some(Scheme::Other(_)) | None => Err(SipError::Unsupported),
        }
    }

    fn transport(self, protocol: Protocol) -> Result<Self, SipError> {
        if self == Self::Sips && !matches!(protocol, Protocol::Tls | Protocol::Wss) {
            Err(SipError::Unsupported)
        } else {
            Ok(self)
        }
    }

    pub(super) fn target(protocol: Protocol, uri: &Uri) -> Result<Self, SipError> {
        let security = Self::address(uri)?.transport(protocol)?;
        if !uri.headers.is_empty()
            || uri
                .auth
                .as_ref()
                .is_some_and(|auth| auth.password.is_some())
        {
            return Err(SipError::Unsupported);
        }
        for parameter in &uri.params {
            let allowed = match parameter {
                Param::Transport(value) => matches!(
                    (protocol, value),
                    (Protocol::Udp, Transport::Udp)
                        | (Protocol::Tcp | Protocol::Tls, Transport::Tcp)
                        | (Protocol::Ws | Protocol::Wss, Transport::Ws)
                ),
                Param::Other(name, value)
                    if crate::contact::parameter_eq(&name.to_string(), "transport") =>
                {
                    value.as_ref().is_some_and(|value| {
                        crate::contact::parameter_eq(
                            &value.to_string(),
                            match protocol {
                                Protocol::Udp => "udp",
                                Protocol::Tcp | Protocol::Tls => "tcp",
                                Protocol::Ws | Protocol::Wss => "ws",
                            },
                        )
                    })
                }
                Param::Other(name, _)
                    if ["method", "maddr", "ttl"].iter().any(|parameter| {
                        crate::contact::parameter_eq(&name.to_string(), parameter)
                    }) =>
                {
                    false
                }
                Param::Method(_) | Param::Ttl(_) | Param::Maddr(_) => false,
                Param::User(_)
                | Param::Lr
                | Param::Ob
                | Param::Rport(_)
                | Param::Branch(_)
                | Param::Received(_)
                | Param::Tag(_)
                | Param::Expires(_)
                | Param::Q(_)
                | Param::Other(_, _) => true,
            };
            if !allowed {
                return Err(SipError::Unsupported);
            }
        }
        Ok(security)
    }

    pub(super) fn incoming(
        protocol: Protocol,
        request: &Request,
    ) -> Result<IncomingTarget, IncomingSecurityError> {
        let target =
            Self::address(&request.uri).map_err(|_| IncomingSecurityError::UnsupportedScheme)?;
        let contacts = crate::parse_registration_contacts(
            request
                .headers
                .iter()
                .filter(|header| header.name().eq_ignore_ascii_case("Contact"))
                .map(|header| header.value()),
            None,
        )?;
        let RegistrationContacts::Bindings(contacts) = contacts else {
            return Err(IncomingSecurityError::Invalid);
        };
        let [contact] = contacts.as_slice() else {
            return Err(IncomingSecurityError::Invalid);
        };
        let contact_security =
            Self::address(contact.uri()).map_err(|_| IncomingSecurityError::Invalid)?;
        if target == Self::Sips && contact_security != Self::Sips {
            return Err(IncomingSecurityError::Invalid);
        }
        let routes = crate::parse_registration_contacts(
            request
                .headers
                .iter()
                .filter(|header| header.name().eq_ignore_ascii_case("Record-Route"))
                .map(|header| header.value()),
            None,
        )?;
        let route = match &routes {
            RegistrationContacts::Query => contact_security,
            RegistrationContacts::Bindings(routes) => Self::address(
                routes
                    .as_slice()
                    .first()
                    .ok_or(IncomingSecurityError::Invalid)?
                    .uri(),
            )
            .map_err(|_| IncomingSecurityError::Invalid)?,
            RegistrationContacts::RemoveAll => return Err(IncomingSecurityError::Invalid),
        };
        let security = if target == Self::Sips || route == Self::Sips {
            Self::Sips
                .transport(protocol)
                .map_err(|_| IncomingSecurityError::SecureTransportRequired)?
        } else {
            Self::Sip
        };
        let target = DialogTarget::new(protocol, security, contact)
            .map_err(|_| IncomingSecurityError::Invalid)?;
        Ok(IncomingTarget { security, target })
    }
}

pub(super) fn token(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Udp => "UDP",
        Protocol::Tcp => "TCP",
        Protocol::Tls => "TLS",
        Protocol::Ws => "WS",
        Protocol::Wss => "WSS",
    }
}

pub(super) fn contact(protocol: Protocol, local: SocketAddr, security: UriSecurity) -> String {
    let parameter = match protocol {
        Protocol::Udp => "",
        Protocol::Tcp => ";transport=tcp",
        Protocol::Tls => ";transport=tcp",
        Protocol::Ws => ";transport=ws",
        Protocol::Wss => ";transport=ws",
    };
    let scheme = match security {
        UriSecurity::Sip => "sip",
        UriSecurity::Sips => "sips",
    };
    format!("<{scheme}:rusterisk@{local}{parameter}>")
}
