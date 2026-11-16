use super::{
    request::{expiration, header},
    RegistrationResponse,
};
use crate::{registration_client::RegistrationIdentity, RegistrationTransaction, SipError};
use rusterisk_config::Auth;
use std::num::NonZeroU32;

impl RegistrationResponse {
    pub fn minimum_expiration(&self) -> Result<Option<NonZeroU32>, SipError> {
        if self.status() != 423 {
            return Ok(None);
        }
        let source = std::str::from_utf8(&self.request.bytes)?;
        let requested = expiration(header(source, "Expires"))?.ok_or(SipError::InvalidPacket)?;
        if requested == 0 {
            return Ok(None);
        }
        let minimum =
            expiration(self.header_values("Min-Expires"))?.ok_or(SipError::InvalidPacket)?;
        if minimum <= requested {
            return Err(SipError::InvalidPacket);
        }
        NonZeroU32::new(minimum)
            .map(Some)
            .ok_or(SipError::InvalidPacket)
    }

    pub fn minimum_expiration_retry(
        self,
        credentials: &[Auth],
        identity: RegistrationIdentity,
        now: u64,
    ) -> Result<Option<RegistrationTransaction>, SipError> {
        let Some(minimum) = self.minimum_expiration()? else {
            return Ok(None);
        };
        self.with_expiration(credentials, identity, minimum.get(), now)
            .map(Some)
    }
}
