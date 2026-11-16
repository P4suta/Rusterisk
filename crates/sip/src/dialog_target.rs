use crate::{
    transport::UriSecurity, ContactError, RegistrationContact, RegistrationContacts, SipError,
};
use rsipstack::sip::{uri::Param, Request, Response, Uri};
use rusterisk_config::Protocol;

#[derive(Clone)]
pub(crate) struct DialogTarget {
    uri: Uri,
}

impl DialogTarget {
    pub(crate) fn new(
        protocol: Protocol,
        required: UriSecurity,
        contact: &RegistrationContact,
    ) -> Result<Self, SipError> {
        let mut uri = contact.uri().clone();
        uri.headers.clear();
        uri.params.retain(|parameter| {
            !matches!(parameter, Param::Method(_) | Param::Ttl(_))
                && !matches!(parameter, Param::Other(name, _) if ["method", "ttl"].iter().any(|parameter| crate::contact::parameter_eq(&name.to_string(), parameter)))
        });
        let security = UriSecurity::target(protocol, &uri)?;
        if required == UriSecurity::Sips && security != UriSecurity::Sips {
            return Err(SipError::Unsupported);
        }
        Ok(Self { uri })
    }

    pub(crate) fn from_request(
        protocol: Protocol,
        required: UriSecurity,
        request: &Request,
    ) -> Result<Option<Self>, SipError> {
        Self::parse(
            protocol,
            required,
            request
                .headers
                .iter()
                .filter(|header| header.name().eq_ignore_ascii_case("Contact"))
                .map(|header| header.value()),
        )
    }

    pub(crate) fn from_response(
        protocol: Protocol,
        required: UriSecurity,
        response: &Response,
    ) -> Result<Option<Self>, SipError> {
        Self::parse(
            protocol,
            required,
            response
                .headers
                .iter()
                .filter(|header| header.name().eq_ignore_ascii_case("Contact"))
                .map(|header| header.value()),
        )
    }

    pub(crate) fn from_uri(
        protocol: Protocol,
        required: UriSecurity,
        uri: &Uri,
    ) -> Result<Self, SipError> {
        let source = format!("<{uri}>");
        Self::parse(protocol, required, [source.as_str()])?.ok_or(SipError::InvalidPacket)
    }

    fn parse<'a>(
        protocol: Protocol,
        required: UriSecurity,
        values: impl IntoIterator<Item = &'a str>,
    ) -> Result<Option<Self>, SipError> {
        let contacts =
            crate::parse_registration_contacts(values, None).map_err(|error| match error {
                ContactError::Invalid => SipError::InvalidPacket,
                ContactError::Unsupported => SipError::Unsupported,
                ContactError::Capacity => SipError::Capacity,
            })?;
        match contacts {
            RegistrationContacts::Query => Ok(None),
            RegistrationContacts::Bindings(contacts) => {
                let [contact] = contacts.as_slice() else {
                    return Err(SipError::InvalidPacket);
                };
                Self::new(protocol, required, contact).map(Some)
            }
            RegistrationContacts::RemoveAll => Err(SipError::InvalidPacket),
        }
    }

    pub(crate) fn uri(&self) -> &Uri {
        &self.uri
    }
}
