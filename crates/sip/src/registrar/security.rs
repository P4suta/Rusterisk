use super::Rejection;
use crate::RegistrationContacts;
use rsipstack::sip::{uri::Scheme, Request};

pub(super) fn validate(
    request: &Request,
    contacts: &RegistrationContacts,
) -> Result<(), Rejection> {
    let RegistrationContacts::Bindings(contacts) = contacts else {
        return Ok(());
    };
    let secure = contacts
        .as_slice()
        .iter()
        .any(|contact| matches!(contact.uri().scheme, Some(Scheme::Sips)));
    let insecure = contacts
        .as_slice()
        .iter()
        .any(|contact| !matches!(contact.uri().scheme, Some(Scheme::Sips)));
    let secure_target = matches!(request.uri.scheme, Some(Scheme::Sips));
    if secure_target && insecure || secure && !secure_target {
        return Err(Rejection::Invalid);
    }
    if !secure {
        return Ok(());
    }
    let paths = crate::contact::parse_paths(
        request
            .headers
            .iter()
            .filter(|header| header.name().eq_ignore_ascii_case("Path"))
            .map(|header| header.value()),
    )
    .map_err(|_| Rejection::Invalid)?;
    if paths
        .iter()
        .all(|path| matches!(path.scheme, Some(Scheme::Sips)))
    {
        Ok(())
    } else {
        Err(Rejection::Invalid)
    }
}
