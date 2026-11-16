use crate::{registration, Protocol};
use rsipstack::sip::{uri::Host, Scheme, Transport, Uri};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationUriError {
    Invalid,
    Unsupported,
    TransportConflict,
    Capacity,
}

impl std::fmt::Display for RegistrationUriError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "invalid registration URI",
            Self::Unsupported => "unsupported registration URI component or transport",
            Self::TransportConflict => "registration URI conflicts with its transport",
            Self::Capacity => "registration URI exceeds 2048 bytes",
        })
    }
}

impl std::error::Error for RegistrationUriError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrarUri {
    source: String,
    protocol: Protocol,
    secure: bool,
}

impl RegistrarUri {
    pub fn parse(protocol: Protocol, source: &str) -> Result<Self, RegistrationUriError> {
        if matches!(protocol, Protocol::Ws | Protocol::Wss) {
            return Err(RegistrationUriError::Unsupported);
        }
        let profile = profile(source, Role::Registrar)?;
        if profile.secure && protocol != Protocol::Tls {
            return Err(RegistrationUriError::TransportConflict);
        }
        if let Some(selected) = profile.transport {
            if selected == Transport::Tls {
                return Err(RegistrationUriError::Unsupported);
            }
            let matching = matches!(
                (protocol, selected),
                (Protocol::Udp, Transport::Udp) | (Protocol::Tcp | Protocol::Tls, Transport::Tcp)
            );
            if !matching {
                return Err(RegistrationUriError::TransportConflict);
            }
        }
        Ok(Self {
            source: source.into(),
            protocol,
            secure: profile.secure,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    pub const fn protocol(&self) -> Protocol {
        self.protocol
    }

    pub const fn is_secure(&self) -> bool {
        self.secure
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrationAddress {
    source: String,
    secure: bool,
}

impl RegistrationAddress {
    pub fn parse(source: &str) -> Result<Self, RegistrationUriError> {
        let profile = profile(source, Role::Address)?;
        Ok(Self {
            source: source.into(),
            secure: profile.secure,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    pub const fn is_secure(&self) -> bool {
        self.secure
    }
}

#[derive(Clone, Copy)]
enum Role {
    Registrar,
    Address,
}

struct Profile {
    secure: bool,
    transport: Option<Transport>,
}

fn profile(source: &str, role: Role) -> Result<Profile, RegistrationUriError> {
    if source.len() > 2048 {
        return Err(RegistrationUriError::Capacity);
    }
    if source.is_empty()
        || source
            .bytes()
            .any(|byte| !byte.is_ascii_graphic() || b"<>\"\\#".contains(&byte))
    {
        return Err(RegistrationUriError::Invalid);
    }
    let uri = Uri::try_from(source).map_err(|_| RegistrationUriError::Invalid)?;
    let secure = match &uri.scheme {
        Some(Scheme::Sip) => false,
        Some(Scheme::Sips) => true,
        Some(Scheme::Other(_)) | None => return Err(RegistrationUriError::Unsupported),
    };
    let (_, body) = source
        .split_once(':')
        .ok_or(RegistrationUriError::Invalid)?;
    let authority = match body.rsplit_once('@') {
        Some((user, authority)) => {
            if matches!(role, Role::Registrar) || user.is_empty() || !registration::valid_user(user)
            {
                return Err(RegistrationUriError::Invalid);
            }
            authority
        }
        None => body,
    };
    if authority.contains('?') || !uri.headers.is_empty() {
        return Err(RegistrationUriError::Unsupported);
    }
    let (host, parameters) = authority
        .split_once(';')
        .map_or((authority, None), |(host, parameters)| {
            (host, Some(parameters))
        });
    validate_host(host, &uri, role)?;
    let transport = parameters
        .map(|parameters| validate_parameters(parameters, role))
        .transpose()?
        .flatten();
    Ok(Profile { secure, transport })
}

fn validate_host(source: &str, uri: &Uri, role: Role) -> Result<(), RegistrationUriError> {
    if uri.host_with_port.port.is_some_and(|port| port.0 == 0) {
        return Err(RegistrationUriError::Invalid);
    }
    if matches!(role, Role::Address) && uri.host_with_port.port.is_some() {
        return Err(RegistrationUriError::Unsupported);
    }
    match &uri.host_with_port.host {
        Host::IpAddr(address) => {
            if source.starts_with('[') != address.is_ipv6() {
                return Err(RegistrationUriError::Invalid);
            }
            if matches!(role, Role::Registrar)
                && (address.is_unspecified() || address.is_multicast())
            {
                return Err(RegistrationUriError::Invalid);
            }
            if address.is_ipv6() {
                let suffix = source
                    .strip_prefix('[')
                    .and_then(|source| source.split_once(']'))
                    .map(|(_, suffix)| suffix)
                    .ok_or(RegistrationUriError::Invalid)?;
                if !suffix.is_empty()
                    && !suffix.strip_prefix(':').is_some_and(|port| {
                        !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
                    })
                {
                    return Err(RegistrationUriError::Invalid);
                }
            }
        }
        Host::Domain(name) => {
            if !valid_domain(&name.0) {
                return Err(RegistrationUriError::Invalid);
            }
        }
    }
    Ok(())
}

fn valid_domain(source: &str) -> bool {
    let domain = source.strip_suffix('.').unwrap_or(source);
    if domain.is_empty() || domain.len() > 253 {
        return false;
    }
    let mut labels = domain.split('.').peekable();
    while let Some(label) = labels.next() {
        let bytes = label.as_bytes();
        if label.len() > 63
            || !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
            || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
            || labels.peek().is_none() && !bytes.first().is_some_and(u8::is_ascii_alphabetic)
        {
            return false;
        }
    }
    true
}

fn validate_parameters(
    source: &str,
    role: Role,
) -> Result<Option<Transport>, RegistrationUriError> {
    let mut names = BTreeSet::new();
    let mut transport = None;
    for parameter in source.split(';') {
        let (name, value) = parameter
            .split_once('=')
            .map_or((parameter, None), |(name, value)| (name, Some(value)));
        let name = component(name)?;
        if !names.insert(name.clone()) || value.is_some_and(|value| component(value).is_err()) {
            return Err(RegistrationUriError::Invalid);
        }
        let name: String = name.into_iter().map(|(_, byte)| char::from(byte)).collect();
        match name.as_str() {
            "transport" => match role {
                Role::Registrar => {
                    transport = Some(
                        value
                            .ok_or(RegistrationUriError::Invalid)?
                            .parse()
                            .map_err(|_| RegistrationUriError::Unsupported)?,
                    );
                }
                Role::Address => return Err(RegistrationUriError::Unsupported),
            },
            "method" | "maddr" | "ttl" | "lr" => {
                return Err(RegistrationUriError::Unsupported);
            }
            "user"
                if !value.is_some_and(|value| {
                    value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-.!%*_+`'~".contains(&byte))
                }) =>
            {
                return Err(RegistrationUriError::Invalid);
            }
            _ => {}
        }
    }
    Ok(transport)
}

fn component(source: &str) -> Result<Vec<(bool, u8)>, RegistrationUriError> {
    if source.is_empty() {
        return Err(RegistrationUriError::Invalid);
    }
    let mut output = Vec::with_capacity(source.len());
    let mut bytes = source.bytes();
    while let Some(byte) = bytes.next() {
        let (reserved_escape, byte) = if byte == b'%' {
            let high = bytes.next().and_then(hexadecimal);
            let low = bytes.next().and_then(hexadecimal);
            let (Some(high), Some(low)) = (high, low) else {
                return Err(RegistrationUriError::Invalid);
            };
            let decoded = high * 16 + low;
            (b";/?:@&=+$,".contains(&decoded), decoded)
        } else {
            if !byte.is_ascii_alphanumeric() && !b"-_.!~*'()[]/:&+$".contains(&byte) {
                return Err(RegistrationUriError::Invalid);
            }
            (false, byte)
        };
        output.push((reserved_escape, byte.to_ascii_lowercase()));
    }
    Ok(output)
}

fn hexadecimal(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
