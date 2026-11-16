use super::{entries, identity, parts, ContactError};
use rsipstack::sip::{headers::typed::RecordRoute, uri::Scheme, Uri};

pub(crate) fn parse_paths<'a>(
    values: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<Uri>, ContactError> {
    let entries = entries(values)?;
    let mut paths = Vec::with_capacity(entries.len());
    for entry in entries {
        let (prefix, raw_uri, _) = parts(entry)?;
        if !prefix.ends_with('>') {
            return Err(ContactError::Invalid);
        }
        if raw_uri.len() > 2048 {
            return Err(ContactError::Capacity);
        }
        identity::validate(raw_uri)?;
        let route = RecordRoute::parse(entry).map_err(|_| ContactError::Invalid)?;
        if !route.has_lr() {
            return Err(ContactError::Invalid);
        }
        if route.params.len() > 32 {
            return Err(ContactError::Capacity);
        }
        let uri = route.uri;
        if !matches!(uri.scheme, Some(Scheme::Sip | Scheme::Sips))
            || uri
                .auth
                .as_ref()
                .is_some_and(|auth| auth.password.is_some())
        {
            return Err(ContactError::Unsupported);
        }
        if uri.params.len() > 32 || uri.headers.len() > 16 {
            return Err(ContactError::Capacity);
        }
        paths.push(uri);
    }
    Ok(paths)
}
