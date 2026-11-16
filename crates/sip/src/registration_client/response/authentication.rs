use super::RegistrationResponse;
use crate::{
    digest::{DigestError, Header, Protection},
    registration_client::{RegistrationIdentity, RegistrationTransaction, State, TIMEOUT},
    Datagram, SipError, MAX_MESSAGE,
};
use rusterisk_config::Auth;

enum Scope {
    Origin,
    Proxy,
}

impl Scope {
    fn challenge(&self) -> &'static str {
        match self {
            Self::Origin => "WWW-Authenticate",
            Self::Proxy => "Proxy-Authenticate",
        }
    }
    fn authorization(&self) -> &'static str {
        match self {
            Self::Origin => "Authorization",
            Self::Proxy => "Proxy-Authorization",
        }
    }
    fn other_authorization(&self) -> &'static str {
        match self {
            Self::Origin => "Proxy-Authorization",
            Self::Proxy => "Authorization",
        }
    }
}

impl RegistrationResponse {
    pub fn authenticated_retry(
        self,
        credentials: &[Auth],
        identity: RegistrationIdentity,
        client_nonce: &str,
        nonce_count: u32,
        now: u64,
    ) -> Result<Option<RegistrationTransaction>, SipError> {
        let scope = match self.status() {
            401 => Scope::Origin,
            407 => Scope::Proxy,
            _ => return Ok(None),
        };
        if now < self.received
            || identity.boot != self.identity.boot
            || identity.sequence <= self.identity.sequence
        {
            return Err(SipError::InvalidPacket);
        }
        let expires = now.checked_add(TIMEOUT).ok_or(SipError::Capacity)?;
        let count = format!("{nonce_count:08x}");
        Protection::parse(Some("auth"), Some(client_nonce), Some(&count)).map_err(digest_error)?;
        if self.authentication_attempts >= 4 {
            return Ok(None);
        }
        let source = std::str::from_utf8(&self.request.bytes)?;
        let uri = self.request_uri()?;
        let (_, body) = source
            .split_once("\r\n\r\n")
            .ok_or(SipError::InvalidPacket)?;
        let previous = header(source, scope.authorization());
        let selected = match Header::choose_challenge(
            self.header_values(scope.challenge()),
            credentials,
            previous,
        ) {
            Ok(Some(selected)) => selected,
            Ok(None) | Err(DigestError::Unsupported) => return Ok(None),
            Err(DigestError::Invalid) => return Err(SipError::InvalidPacket),
        };
        let authorization = selected
            .authorization("REGISTER", uri, body.as_bytes(), client_nonce, nonce_count)
            .map_err(digest_error)?;
        let other_name = scope.other_authorization();
        let other = if let Some(previous) = header(source, other_name) {
            let previous = Header::parse(previous).map_err(digest_error)?;
            if previous.authorization().map_err(digest_error)?.count() == u32::MAX {
                return Err(SipError::Capacity);
            }
            let Some(renewed) = previous
                .renew_authorization(credentials, "REGISTER", uri, body.as_bytes())
                .map_err(digest_error)?
            else {
                return Ok(None);
            };
            Some(renewed)
        } else {
            None
        };
        let bytes = render(source, &scope, &authorization, other.as_deref(), identity)?;
        Ok(Some(RegistrationTransaction {
            identity,
            packet: Datagram {
                peer: self.request.peer,
                bytes,
            },
            protocol: self.protocol,
            expires,
            observed: now,
            state: State::Ready { at: now },
            authentication_attempts: self.authentication_attempts + 1,
        }))
    }
}

fn header<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    source
        .split_once("\r\n\r\n")?
        .0
        .split("\r\n")
        .filter_map(|line| line.split_once(':'))
        .find_map(|(field, value)| {
            field
                .eq_ignore_ascii_case(name)
                .then(|| value.trim_matches([' ', '\t']))
        })
}

fn render(
    source: &str,
    scope: &Scope,
    authorization: &str,
    other: Option<&str>,
    identity: RegistrationIdentity,
) -> Result<Vec<u8>, SipError> {
    let (headers, body) = source
        .split_once("\r\n\r\n")
        .ok_or(SipError::InvalidPacket)?;
    let mut output = String::with_capacity(source.len().min(MAX_MESSAGE));
    for (index, line) in headers.split("\r\n").enumerate() {
        if index == 0 {
            output.push_str(line);
        } else {
            let (name, value) = line.split_once(':').ok_or(SipError::InvalidPacket)?;
            if name.eq_ignore_ascii_case(scope.authorization())
                || name.eq_ignore_ascii_case(scope.other_authorization())
            {
                continue;
            }
            if name.eq_ignore_ascii_case("Via") {
                let (sent_by, _) = value
                    .rsplit_once(";branch=")
                    .ok_or(SipError::InvalidPacket)?;
                output.push_str(&format!(
                    "Via:{sent_by};branch=z9hG4bK-reg-{:032x}-{}",
                    identity.branch, identity.sequence
                ));
            } else if name.eq_ignore_ascii_case("CSeq") {
                output.push_str(&format!("CSeq: {} REGISTER", identity.sequence));
            } else {
                output.push_str(line);
            }
        }
        output.push_str("\r\n");
    }
    output.push_str(scope.authorization());
    output.push_str(": ");
    output.push_str(authorization);
    output.push_str("\r\n");
    if let Some(other) = other {
        output.push_str(scope.other_authorization());
        output.push_str(": ");
        output.push_str(other);
        output.push_str("\r\n");
    }
    output.push_str("\r\n");
    output.push_str(body);
    if output.len() > MAX_MESSAGE {
        return Err(SipError::Capacity);
    }
    Ok(output.into_bytes())
}

fn digest_error(error: DigestError) -> SipError {
    match error {
        DigestError::Invalid => SipError::InvalidPacket,
        DigestError::Unsupported => SipError::Unsupported,
    }
}
