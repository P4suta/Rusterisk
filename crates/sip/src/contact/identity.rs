use super::{ContactError, RegistrationContact};
use rsipstack::sip::uri::Host;

struct Parts<'a> {
    user: Option<&'a str>,
    host: &'a str,
    parameters: Option<&'a str>,
}

fn parts(source: &str) -> Option<Parts<'_>> {
    let (_, rest) = source.split_once(':')?;
    let (user, rest) = match rest.rsplit_once('@') {
        Some((user, rest)) => (Some(user), rest),
        None => (None, rest),
    };
    let authority = rest
        .split_once('?')
        .map_or(rest, |(authority, _)| authority);
    let (host, parameters) = match authority.split_once(';') {
        Some((host, parameters)) => (host, Some(parameters)),
        None => (authority, None),
    };
    Some(Parts {
        user,
        host,
        parameters,
    })
}

fn parameters(source: Option<&str>) -> impl Iterator<Item = (&str, Option<&str>)> {
    source
        .into_iter()
        .flat_map(|source| source.split(';'))
        .map(|part| match part.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (part, None),
        })
}

fn hexadecimal(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn next(source: &mut std::slice::Iter<'_, u8>, fold: bool) -> Option<(bool, u8)> {
    let byte = *source.next()?;
    let (escaped, byte) = if byte == b'%' {
        let high = hexadecimal(*source.next()?)?;
        let low = hexadecimal(*source.next()?)?;
        let byte = high * 16 + low;
        (b";/?:@&=+$,".contains(&byte), byte)
    } else {
        (false, byte)
    };
    Some((
        escaped,
        if fold {
            byte.to_ascii_lowercase()
        } else {
            byte
        },
    ))
}

fn component(left: &str, right: &str, fold: bool) -> bool {
    let mut left = left.as_bytes().iter();
    let mut right = right.as_bytes().iter();
    loop {
        match (next(&mut left, fold), next(&mut right, fold)) {
            (None, None) => return true,
            (Some(left), Some(right)) if left == right => {}
            (None, Some(_)) | (Some(_), None) | (Some(_), Some(_)) => return false,
        }
    }
}

fn optional(left: Option<&str>, right: Option<&str>, fold: bool) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => component(left, right, fold),
        (None, None) => true,
        (None, Some(_)) | (Some(_), None) => false,
    }
}

fn significant(name: &str) -> bool {
    ["transport", "user", "ttl", "method", "maddr"]
        .into_iter()
        .any(|known| component(name, known, true))
}

pub(super) fn validate(source: &str) -> Result<(), ContactError> {
    let mut bytes = source.as_bytes().iter();
    while let Some(byte) = bytes.next() {
        if *byte == b'%' {
            let high = bytes.next().copied().and_then(hexadecimal);
            let low = bytes.next().copied().and_then(hexadecimal);
            if high.is_none() || low.is_none() {
                return Err(ContactError::Invalid);
            }
        }
    }
    let parts = parts(source).ok_or(ContactError::Invalid)?;
    if parts.host.contains('%')
        || parts
            .user
            .is_some_and(|user| user.is_empty() || user.contains('@'))
    {
        return Err(ContactError::Invalid);
    }
    for (index, (name, value)) in parameters(parts.parameters).enumerate() {
        if name.is_empty()
            || value == Some("")
            || parameters(parts.parameters)
                .take(index)
                .any(|(prior, _)| component(prior, name, true))
        {
            return Err(ContactError::Invalid);
        }
    }
    Ok(())
}

pub(super) fn equivalent(left: &RegistrationContact, right: &RegistrationContact) -> bool {
    let (Some(left_parts), Some(right_parts)) =
        (parts(left.source_uri()), parts(right.source_uri()))
    else {
        return false;
    };
    let (left, right) = (left.uri(), right.uri());
    if left.scheme != right.scheme
        || left.host_with_port.port != right.host_with_port.port
        || !optional(left_parts.user, right_parts.user, false)
    {
        return false;
    }
    let host = match (&left.host_with_port.host, &right.host_with_port.host) {
        (Host::Domain(left), Host::Domain(right)) => left.0.eq_ignore_ascii_case(&right.0),
        (Host::IpAddr(left), Host::IpAddr(right)) => left == right,
        (Host::Domain(_), Host::IpAddr(_)) | (Host::IpAddr(_), Host::Domain(_)) => false,
    };
    if !host {
        return false;
    }
    for (name, value) in parameters(left_parts.parameters) {
        match parameters(right_parts.parameters).find(|(other, _)| component(name, other, true)) {
            Some((_, other)) if !optional(value, other, !component(name, "method", true)) => {
                return false
            }
            None if significant(name) => return false,
            Some(_) | None => {}
        }
    }
    if parameters(right_parts.parameters).any(|(name, _)| {
        significant(name)
            && !parameters(left_parts.parameters).any(|(other, _)| component(name, other, true))
    }) {
        return false;
    }
    left.headers.len() == right.headers.len()
        && left.headers.iter().all(|field| {
            left.headers
                .iter()
                .filter(|other| header(field, other))
                .count()
                == right
                    .headers
                    .iter()
                    .filter(|other| header(field, other))
                    .count()
        })
}

fn header(left: &(String, String), right: &(String, String)) -> bool {
    component(&left.0, &right.0, true)
        && component(&left.1, &right.1, component(&left.0, "priority", true))
}
