use super::{
    request::{digest_error, expiration, header, render, RequestUpdate},
    RegistrationGrant, RegistrationResponse,
};
use crate::{
    digest::Header,
    registration_client::{RegistrationIdentity, RegistrationTransaction, State, TIMEOUT},
    Datagram, SipError,
};
use rusterisk_config::Auth;
use std::num::NonZeroU32;

impl RegistrationResponse {
    pub fn retry_after_failure(
        &self,
        credentials: &[Auth],
        identity: RegistrationIdentity,
        now: u64,
    ) -> Result<RegistrationTransaction, SipError> {
        if !(300..=699).contains(&self.status()) || self.status() == 423 {
            return Err(SipError::Unsupported);
        }
        let source = std::str::from_utf8(&self.request.bytes)?;
        let expiration = expiration(header(source, "Expires"))?.ok_or(SipError::InvalidPacket)?;
        let mut transaction = self.with_expiration(credentials, identity, expiration, now)?;
        transaction.authentication.attempts = 0;
        Ok(transaction)
    }

    pub fn refresh(
        &self,
        credentials: &[Auth],
        identity: RegistrationIdentity,
        expiration: NonZeroU32,
        now: u64,
    ) -> Result<RegistrationTransaction, SipError> {
        self.update(credentials, identity, expiration.get(), now)
    }

    pub fn remove(
        &self,
        credentials: &[Auth],
        identity: RegistrationIdentity,
        now: u64,
    ) -> Result<RegistrationTransaction, SipError> {
        self.update(credentials, identity, 0, now)
    }

    fn update(
        &self,
        credentials: &[Auth],
        identity: RegistrationIdentity,
        expiration: u32,
        now: u64,
    ) -> Result<RegistrationTransaction, SipError> {
        if !matches!(self.grant()?, RegistrationGrant::Active { .. }) {
            return Err(SipError::Unsupported);
        }
        let mut transaction = self.with_expiration(credentials, identity, expiration, now)?;
        transaction.authentication.attempts = 0;
        Ok(transaction)
    }

    pub(super) fn with_expiration(
        &self,
        credentials: &[Auth],
        identity: RegistrationIdentity,
        expiration: u32,
        now: u64,
    ) -> Result<RegistrationTransaction, SipError> {
        if now < self.received
            || identity.boot != self.identity.boot
            || identity.sequence <= self.identity.sequence
        {
            return Err(SipError::InvalidPacket);
        }
        let expires = now.checked_add(TIMEOUT).ok_or(SipError::Capacity)?;
        let source = std::str::from_utf8(&self.request.bytes)?;
        let uri = self.request_uri()?;
        let (_, body) = source
            .split_once("\r\n\r\n")
            .ok_or(SipError::InvalidPacket)?;
        let authorization = renew(
            source,
            "Authorization",
            credentials,
            uri,
            body.as_bytes(),
            self.authentication.origin.encoding(),
        )?;
        let proxy = renew(
            source,
            "Proxy-Authorization",
            credentials,
            uri,
            body.as_bytes(),
            self.authentication.proxy.encoding(),
        )?;
        let bytes = render(
            source,
            RequestUpdate {
                identity,
                expiration: Some(expiration),
                authorization: authorization.as_deref(),
                proxy_authorization: proxy.as_deref(),
            },
        )?;
        Ok(RegistrationTransaction {
            identity,
            packet: Datagram::new(self.request.peer, bytes),
            protocol: self.protocol,
            expires,
            observed: now,
            state: State::Ready { at: now },
            authentication: self.authentication,
        })
    }
}

fn renew(
    source: &str,
    name: &str,
    credentials: &[Auth],
    uri: &str,
    body: &[u8],
    encoding: crate::digest::CredentialEncoding,
) -> Result<Option<String>, SipError> {
    let Some(previous) = header(source, name) else {
        return Ok(None);
    };
    let previous = Header::parse(previous).map_err(digest_error)?;
    if previous.authorization().map_err(digest_error)?.count() == u32::MAX {
        return Err(SipError::Capacity);
    }
    previous
        .renew_authorization(credentials, "REGISTER", uri, body, encoding)
        .map_err(digest_error)?
        .map(Some)
        .ok_or(SipError::Unsupported)
}
