use super::RegistrationResponse;
use crate::{
    contact::parse_response_contacts, parse_registration_contacts, ContactError,
    RegistrationContacts, SipError,
};
use rsipstack::sip::SipMessage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationGrant {
    Active { expires_at: u64, refresh_at: u64 },
    Removed,
}

impl RegistrationResponse {
    pub fn grant(&self) -> Result<RegistrationGrant, SipError> {
        if !(200..300).contains(&self.status()) {
            return Err(SipError::Unsupported);
        }
        let request = match SipMessage::try_from(self.request.bytes.as_slice())? {
            SipMessage::Request(request) => request,
            SipMessage::Response(_) => return Err(SipError::InvalidPacket),
        };
        let requested = expiration(
            request
                .headers
                .iter()
                .filter(|header| header.name().eq_ignore_ascii_case("Expires"))
                .map(|header| header.value()),
        )?
        .ok_or(SipError::InvalidPacket)?;
        let RegistrationContacts::Bindings(owned) = parse_registration_contacts(
            request
                .headers
                .iter()
                .filter(|header| header.name().eq_ignore_ascii_case("Contact"))
                .map(|header| header.value()),
            None,
        )
        .map_err(contact_error)?
        else {
            return Err(SipError::InvalidPacket);
        };
        let [owned] = owned.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        let contacts = parse_response_contacts(
            self.header_values("Contact"),
            expiration(self.header_values("Expires"))?,
        )
        .map_err(contact_error)?;
        let mut matching = contacts
            .as_slice()
            .iter()
            .filter(|candidate| candidate.equivalent(owned));
        let matched = matching.next();
        if matching.next().is_some() {
            return Err(SipError::InvalidPacket);
        }
        let granted = match matched {
            None if requested == 0 => return Ok(RegistrationGrant::Removed),
            None => return Err(SipError::InvalidPacket),
            Some(contact) => contact.expires().ok_or(SipError::InvalidPacket)?,
        };
        if requested == 0 {
            return if granted == 0 {
                Ok(RegistrationGrant::Removed)
            } else {
                Err(SipError::InvalidPacket)
            };
        }
        if granted == 0 {
            return Err(SipError::InvalidPacket);
        }
        let expires_at = self
            .received
            .checked_add(u64::from(granted) * 1000)
            .ok_or(SipError::Capacity)?;
        let refresh_at = self
            .received
            .checked_add(u64::from(granted.saturating_sub(10)) * 1000)
            .ok_or(SipError::Capacity)?;
        Ok(RegistrationGrant::Active {
            expires_at,
            refresh_at,
        })
    }
}

fn expiration<'a>(values: impl IntoIterator<Item = &'a str>) -> Result<Option<u32>, SipError> {
    let mut values = values.into_iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some()
        || value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(SipError::InvalidPacket);
    }
    value.parse().map(Some).map_err(|_| SipError::InvalidPacket)
}

fn contact_error(error: ContactError) -> SipError {
    match error {
        ContactError::Invalid => SipError::InvalidPacket,
        ContactError::Unsupported => SipError::Unsupported,
        ContactError::Capacity => SipError::Capacity,
    }
}
