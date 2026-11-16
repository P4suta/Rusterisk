use super::{
    request::{digest_error, header, render, RequestUpdate},
    RegistrationResponse,
};
use crate::{
    digest::{DigestError, Header, Protection},
    registration_client::{RegistrationIdentity, RegistrationTransaction, State, TIMEOUT},
    Datagram, SipError,
};
use rusterisk_config::Auth;

pub enum RegistrationAuthentication {
    Retry(RegistrationTransaction),
    Rejected(RegistrationResponse),
}

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
        self.authenticated_request(credentials, identity, client_nonce, nonce_count, now)
    }

    pub fn authenticate(
        self,
        credentials: &[Auth],
        identity: RegistrationIdentity,
        client_nonce: &str,
        nonce_count: u32,
        now: u64,
    ) -> Result<RegistrationAuthentication, SipError> {
        Ok(
            match self.authenticated_request(
                credentials,
                identity,
                client_nonce,
                nonce_count,
                now,
            )? {
                Some(transaction) => RegistrationAuthentication::Retry(transaction),
                None => RegistrationAuthentication::Rejected(self),
            },
        )
    }

    fn authenticated_request(
        &self,
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
        let (origin, proxy) = match scope {
            Scope::Origin => (Some(authorization.as_str()), other.as_deref()),
            Scope::Proxy => (other.as_deref(), Some(authorization.as_str())),
        };
        let bytes = render(
            source,
            RequestUpdate {
                identity,
                expiration: None,
                authorization: origin,
                proxy_authorization: proxy,
            },
        )?;
        Ok(Some(RegistrationTransaction {
            identity,
            packet: Datagram::new(self.request.peer, bytes),
            protocol: self.protocol,
            expires,
            observed: now,
            state: State::Ready { at: now },
            authentication_attempts: self.authentication_attempts + 1,
        }))
    }
}
