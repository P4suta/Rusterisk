use super::{BlindRefer, ReferError, ReferSubscription};
use rsipstack::sip::{headers::typed::Contact, uri::Host, Method, Request, Uri};
use std::collections::BTreeSet;

pub(super) fn parse(request: &Request) -> Result<BlindRefer, ReferError> {
    if request.method != Method::Refer {
        return Err(ReferError::Invalid);
    }
    let mut total = request.body.len();
    let mut target = None;
    let mut subscription = None;
    for (index, header) in request.headers.iter().enumerate() {
        total = total
            .checked_add(header.name().len())
            .and_then(|total| total.checked_add(header.value().len()))
            .ok_or(ReferError::Capacity)?;
        if index >= 64 || total > crate::MAX_MESSAGE {
            return Err(ReferError::Capacity);
        }
        if header.name().eq_ignore_ascii_case("Refer-To") || header.name().eq_ignore_ascii_case("r")
        {
            if target.replace(header.value()).is_some() {
                return Err(ReferError::Invalid);
            }
        } else if header.name().eq_ignore_ascii_case("Refer-Sub")
            && subscription.replace(header.value()).is_some()
        {
            return Err(ReferError::Invalid);
        }
    }
    let target = target.ok_or(ReferError::Invalid)?;
    let (prefix, uri, parameters) = address(target)?;
    generic_parameters(parameters)?;
    let extension = extension(uri)?;
    Contact::parse(prefix).map_err(|_| ReferError::Invalid)?;
    let subscription = match subscription {
        Some(value) => {
            let value = match value.split_once(';') {
                Some((value, parameters)) => {
                    if parameters.trim_matches([' ', '\t']).is_empty() {
                        return Err(ReferError::Invalid);
                    }
                    generic_parameters(parameters)?;
                    value
                }
                None => value,
            };
            let value = value.trim_matches([' ', '\t']);
            if value.eq_ignore_ascii_case("true") {
                ReferSubscription::Notify
            } else if value.eq_ignore_ascii_case("false") {
                ReferSubscription::Suppressed
            } else {
                return Err(ReferError::Invalid);
            }
        }
        None => ReferSubscription::Notify,
    };
    Ok(BlindRefer {
        extension,
        subscription,
    })
}

fn address(source: &str) -> Result<(&str, &str, &str), ReferError> {
    let source = source.trim_matches([' ', '\t']);
    let mut quoted = false;
    let mut escaped = false;
    for (index, byte) in source.bytes().enumerate() {
        if byte.is_ascii_control() && byte != b'\t' {
            return Err(ReferError::Invalid);
        }
        if escaped {
            escaped = false;
        } else if quoted && byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        } else if !quoted && byte == b'<' {
            let end = source[index + 1..]
                .find('>')
                .map(|end| index + 1 + end)
                .ok_or(ReferError::Invalid)?;
            let suffix = source[end + 1..].trim_matches([' ', '\t']);
            let parameters = match suffix.strip_prefix(';') {
                Some("") => return Err(ReferError::Invalid),
                Some(parameters) => parameters,
                None if suffix.is_empty() => "",
                None => return Err(ReferError::Invalid),
            };
            return Ok((&source[..=end], &source[index + 1..end], parameters));
        } else if !quoted && matches!(byte, b',' | b'>') {
            return Err(ReferError::Invalid);
        }
    }
    if quoted || escaped || source.is_empty() {
        return Err(ReferError::Invalid);
    }
    let (uri, parameters) = source.split_once(';').unwrap_or((source, ""));
    if uri.contains('?') || source.ends_with(';') {
        return Err(ReferError::Invalid);
    }
    Ok((uri, uri, parameters))
}

fn extension(source: &str) -> Result<String, ReferError> {
    uri_user(source, UriPurpose::Refer)
}

pub(super) fn validate_dialog_target(source: &str) -> Result<(), ReferError> {
    uri_user(source, UriPurpose::Dialog).map(|_| ())
}

#[derive(Clone, Copy)]
enum UriPurpose {
    Refer,
    Dialog,
}

fn uri_purpose(name: &str, value: Option<&str>, role: UriPurpose) -> Result<(), ReferError> {
    match role {
        UriPurpose::Refer => purpose(name, value),
        UriPurpose::Dialog
            if name.eq_ignore_ascii_case("method") || name.eq_ignore_ascii_case("Replaces") =>
        {
            Err(ReferError::Unsupported)
        }
        UriPurpose::Dialog => Ok(()),
    }
}

fn uri_user(source: &str, role: UriPurpose) -> Result<String, ReferError> {
    if source
        .bytes()
        .any(|byte| !byte.is_ascii_graphic() || b"<>\"\\#".contains(&byte))
    {
        return Err(ReferError::Invalid);
    }
    let (scheme, rest) = source.split_once(':').ok_or(ReferError::Invalid)?;
    if !scheme.eq_ignore_ascii_case("sip") && !scheme.eq_ignore_ascii_case("sips") {
        return Err(ReferError::Unsupported);
    }
    let (user, authority) = match rest.rsplit_once('@') {
        Some((userinfo, authority)) => {
            let (user, password) = userinfo.split_once(':').unwrap_or((userinfo, ""));
            if user.is_empty() {
                return Err(ReferError::Invalid);
            }
            let user = component(user, b"&=+$,;?/")?;
            component(password, b"&=+$,")?;
            (user, authority)
        }
        None => ("s".into(), rest),
    };
    let (authority, headers) = authority
        .split_once('?')
        .map_or((authority, None), |(authority, headers)| {
            (authority, Some(headers))
        });
    let (host, parameters) = authority
        .split_once(';')
        .map_or((authority, None), |(host, parameters)| {
            (host, Some(parameters))
        });
    if let Some(parameters) = parameters {
        let mut names = BTreeSet::new();
        for (index, entry) in parameters.split(';').enumerate() {
            if index >= 32 {
                return Err(ReferError::Capacity);
            }
            let (name, value) = entry
                .split_once('=')
                .map_or((entry, None), |(name, value)| (name, Some(value)));
            let name = component(name, b"[]/:&+$")?;
            if name.is_empty() || !names.insert(name.to_ascii_lowercase()) {
                return Err(ReferError::Invalid);
            }
            let value = value
                .map(|value| component(value, b"[]/:&+$"))
                .transpose()?;
            if value.as_deref() == Some("") {
                return Err(ReferError::Invalid);
            }
            uri_purpose(&name, value.as_deref(), role)?;
        }
    }
    if let Some(headers) = headers {
        if matches!(role, UriPurpose::Dialog) {
            return Err(ReferError::Unsupported);
        }
        for (index, header) in headers.split('&').enumerate() {
            if index >= 16 {
                return Err(ReferError::Capacity);
            }
            let (name, value) = header.split_once('=').ok_or(ReferError::Invalid)?;
            let name = component(name, b"[]/?:+$")?;
            let value = component(value, b"[]/?:+$")?;
            if name.is_empty() {
                return Err(ReferError::Invalid);
            }
            purpose(&name, Some(&value))?;
        }
    }
    let uri = Uri::try_from(source).map_err(|_| ReferError::Invalid)?;
    validate_host(host, &uri)?;
    Ok(user)
}

fn purpose(name: &str, value: Option<&str>) -> Result<(), ReferError> {
    if name.eq_ignore_ascii_case("Replaces")
        || name.eq_ignore_ascii_case("method") && value != Some("INVITE")
    {
        return Err(ReferError::Unsupported);
    }
    Ok(())
}

fn component(source: &str, extra: &[u8]) -> Result<String, ReferError> {
    let mut decoded = Vec::with_capacity(source.len());
    let mut bytes = source.bytes();
    while let Some(byte) = bytes.next() {
        let byte = if byte == b'%' {
            let high = bytes
                .next()
                .and_then(hexadecimal)
                .ok_or(ReferError::Invalid)?;
            let low = bytes
                .next()
                .and_then(hexadecimal)
                .ok_or(ReferError::Invalid)?;
            high * 16 + low
        } else if byte.is_ascii_alphanumeric()
            || b"-_.!~*'()".contains(&byte)
            || extra.contains(&byte)
        {
            byte
        } else {
            return Err(ReferError::Invalid);
        };
        if byte.is_ascii_control() {
            return Err(ReferError::Invalid);
        }
        decoded.push(byte);
    }
    String::from_utf8(decoded).map_err(|_| ReferError::Invalid)
}

fn hexadecimal(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn validate_host(source: &str, uri: &Uri) -> Result<(), ReferError> {
    let (name, port) = if let Some(source) = source.strip_prefix('[') {
        let (name, suffix) = source.split_once(']').ok_or(ReferError::Invalid)?;
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').ok_or(ReferError::Invalid)?)
        };
        if !matches!(
            uri.host_with_port.host,
            Host::IpAddr(std::net::IpAddr::V6(_))
        ) {
            return Err(ReferError::Invalid);
        }
        (name, port)
    } else {
        source
            .split_once(':')
            .map_or((source, None), |(name, port)| (name, Some(port)))
    };
    if let Some(port) = port {
        if port.is_empty()
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || port.parse::<u16>().is_err()
        {
            return Err(ReferError::Invalid);
        }
    }
    if let Host::Domain(_) = uri.host_with_port.host {
        let mut labels = name.strip_suffix('.').unwrap_or(name).split('.').peekable();
        if name.len() > 254 {
            return Err(ReferError::Invalid);
        }
        while let Some(label) = labels.next() {
            let bytes = label.as_bytes();
            if bytes.len() > 63
                || !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
                || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
                || !bytes
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
                || labels.peek().is_none() && !bytes.first().is_some_and(u8::is_ascii_alphabetic)
            {
                return Err(ReferError::Invalid);
            }
        }
    }
    Ok(())
}

fn generic_parameters(source: &str) -> Result<(), ReferError> {
    visit_parameters(source, purpose)
}

pub(super) fn visit_parameters<'a>(
    source: &'a str,
    mut visit: impl FnMut(&'a str, Option<&'a str>) -> Result<(), ReferError>,
) -> Result<(), ReferError> {
    if source.is_empty() {
        return Ok(());
    }
    let mut quoted = false;
    let mut escaped = false;
    let mut start = 0;
    let mut count = 0;
    for (index, byte) in source.bytes().enumerate().chain([(source.len(), b';')]) {
        if byte.is_ascii_control() && byte != b'\t' {
            return Err(ReferError::Invalid);
        }
        if escaped {
            escaped = false;
        } else if quoted && byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        } else if !quoted && byte == b';' {
            if count >= 32 {
                return Err(ReferError::Capacity);
            }
            let entry = source[start..index].trim_matches([' ', '\t']);
            let (name, value) = entry
                .split_once('=')
                .map_or((entry, None), |(name, value)| {
                    (
                        name.trim_matches([' ', '\t']),
                        Some(value.trim_matches([' ', '\t'])),
                    )
                });
            if name.is_empty() || !name.bytes().all(token) {
                return Err(ReferError::Invalid);
            }
            visit(name, value)?;
            if let Some(value) = value {
                if value.starts_with('"') {
                    let mut escaped = false;
                    let mut end = None;
                    for (index, byte) in value.bytes().enumerate().skip(1) {
                        if escaped {
                            escaped = false;
                        } else if byte == b'\\' {
                            escaped = true;
                        } else if byte == b'"' {
                            end = Some(index + 1);
                            break;
                        }
                    }
                    if end != Some(value.len()) {
                        return Err(ReferError::Invalid);
                    }
                } else if value.is_empty()
                    || !value
                        .bytes()
                        .all(|byte| token(byte) || b"[]:".contains(&byte))
                {
                    return Err(ReferError::Invalid);
                }
            }
            count += 1;
            start = index + 1;
        }
    }
    if quoted || escaped {
        return Err(ReferError::Invalid);
    }
    Ok(())
}

fn token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-.!%*_+`'~".contains(&byte)
}
