use super::ReferAuthority;
use std::net::SocketAddr;

pub(crate) struct Target(rsipstack::sip::Uri);

impl Target {
    pub fn from_subscribe(request: &rsipstack::sip::Request) -> Result<Self, crate::ContactError> {
        let contacts = crate::parse_registration_contacts(
            request
                .headers
                .iter()
                .filter(|header| header.name().eq_ignore_ascii_case("Contact"))
                .map(|header| header.value()),
            None,
        )?;
        let crate::RegistrationContacts::Bindings(contacts) = contacts else {
            return Err(crate::ContactError::Invalid);
        };
        let [contact] = contacts.as_slice() else {
            return Err(crate::ContactError::Invalid);
        };
        super::request::validate_dialog_target(contact.source_uri()).map_err(
            |error| match error {
                super::ReferError::Invalid => crate::ContactError::Invalid,
                super::ReferError::Unsupported => crate::ContactError::Unsupported,
                super::ReferError::Capacity => crate::ContactError::Capacity,
            },
        )?;
        Ok(Self(contact.uri().clone()))
    }

    pub fn uri(&self) -> &rsipstack::sip::Uri {
        &self.0
    }
}

#[derive(Clone)]
pub(crate) struct Dialog {
    pub id: String,
    pub generation: u64,
    pub peer: SocketAddr,
    pub wire: super::wire::Dialog,
    pub contact: String,
    pub authority: ReferAuthority,
    pub usage: Usage,
}

#[derive(Clone, Copy)]
pub(crate) enum Usage {
    Active,
    Ended,
}

pub(crate) enum MatchError {
    Rejected(u16, &'static str),
    Missing,
    Packet(crate::SipError),
}

impl From<crate::SipError> for MatchError {
    fn from(error: crate::SipError) -> Self {
        Self::Packet(error)
    }
}

impl From<rsipstack::sip::Error> for MatchError {
    fn from(error: rsipstack::sip::Error) -> Self {
        Self::Packet(error.into())
    }
}
