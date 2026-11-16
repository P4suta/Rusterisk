use rsipstack::sip::{headers::typed::Contact, uri::Scheme, Uri};
mod identity;
mod routes;
pub(super) use routes::parse_paths;

const MAX_ENTRIES: usize = 32;

pub(super) fn parameter_eq(left: &str, right: &str) -> bool {
    identity::component(left, right, true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactError {
    Invalid,
    Unsupported,
    Capacity,
}

impl std::fmt::Display for ContactError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "Invalid registration contacts",
            Self::Unsupported => "Unsupported registration contact",
            Self::Capacity => "Registration contact capacity exceeded",
        })
    }
}
impl std::error::Error for ContactError {}

#[derive(Clone)]
pub struct RegistrationContact {
    uri: Uri,
    source: String,
    expires: Option<u32>,
    preference: u16,
}

impl std::fmt::Debug for RegistrationContact {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegistrationContact")
            .field("expires", &self.expires)
            .field("preference", &self.preference)
            .finish_non_exhaustive()
    }
}

impl RegistrationContact {
    pub fn uri(&self) -> &Uri {
        &self.uri
    }
    pub fn source_uri(&self) -> &str {
        &self.source
    }
    pub fn expires(&self) -> Option<u32> {
        self.expires
    }
    pub fn preference(&self) -> u16 {
        self.preference
    }

    pub fn equivalent(&self, other: &Self) -> bool {
        identity::equivalent(self, other)
    }
}

#[derive(Clone, Debug)]
pub struct ContactBindings(Vec<RegistrationContact>);

impl ContactBindings {
    pub fn as_slice(&self) -> &[RegistrationContact] {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub enum RegistrationContacts {
    Query,
    RemoveAll,
    Bindings(ContactBindings),
}

pub fn parse_registration_contacts<'a>(
    values: impl IntoIterator<Item = &'a str>,
    expires: Option<u32>,
) -> Result<RegistrationContacts, ContactError> {
    let entries = entries(values)?;
    if entries.is_empty() {
        return Ok(RegistrationContacts::Query);
    }
    if entries.contains(&"*") {
        return if entries.len() == 1 && expires == Some(0) {
            Ok(RegistrationContacts::RemoveAll)
        } else {
            Err(ContactError::Invalid)
        };
    }
    let mut contacts = Vec::with_capacity(entries.len());
    for entry in entries {
        contacts.push(contact(entry, expires)?);
    }
    Ok(RegistrationContacts::Bindings(ContactBindings(contacts)))
}

pub(super) fn parse_response_contacts<'a>(
    values: impl IntoIterator<Item = &'a str>,
    expires: Option<u32>,
) -> Result<ContactBindings, ContactError> {
    let mut contacts = Vec::new();
    for entry in entries(values)? {
        let (_, uri, _) = parts(entry)?;
        let (scheme, rest) = uri.split_once(':').ok_or(ContactError::Invalid)?;
        if scheme.eq_ignore_ascii_case("sip") || scheme.eq_ignore_ascii_case("sips") {
            contacts.push(contact(entry, expires)?);
        } else if rest.is_empty()
            || !scheme
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
            || !scheme
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(&byte))
        {
            return Err(ContactError::Invalid);
        }
    }
    Ok(ContactBindings(contacts))
}

fn entries<'a>(values: impl IntoIterator<Item = &'a str>) -> Result<Vec<&'a str>, ContactError> {
    let mut entries = Vec::new();
    let mut total = 0usize;
    for value in values {
        total = total
            .checked_add(value.len())
            .ok_or(ContactError::Capacity)?;
        if total > crate::MAX_MESSAGE {
            return Err(ContactError::Capacity);
        }
        split(value, ',', &mut entries)?;
    }
    Ok(entries)
}

#[derive(Clone, Copy)]
enum Framing {
    Plain,
    Quoted,
    Escaped,
    Address,
}

fn split<'a>(
    source: &'a str,
    delimiter: char,
    entries: &mut Vec<&'a str>,
) -> Result<(), ContactError> {
    let mut state = Framing::Plain;
    let mut start = 0;
    for (offset, character) in source.char_indices() {
        if character.is_control() && character != '\t' {
            return Err(ContactError::Invalid);
        }
        state = match (state, character) {
            (Framing::Plain, '"') => Framing::Quoted,
            (Framing::Plain, '<') => Framing::Address,
            (Framing::Plain, '>') => return Err(ContactError::Invalid),
            (Framing::Plain, character) if character == delimiter => {
                push(&source[start..offset], entries)?;
                start = offset + character.len_utf8();
                Framing::Plain
            }
            (Framing::Plain, _) => Framing::Plain,
            (Framing::Quoted, '\\') => Framing::Escaped,
            (Framing::Quoted, '"') => Framing::Plain,
            (Framing::Quoted, _) | (Framing::Escaped, _) => Framing::Quoted,
            (Framing::Address, '>') => Framing::Plain,
            (Framing::Address, '<' | '"') => return Err(ContactError::Invalid),
            (Framing::Address, _) => Framing::Address,
        };
    }
    if !matches!(state, Framing::Plain) {
        return Err(ContactError::Invalid);
    }
    push(&source[start..], entries)
}

fn push<'a>(source: &'a str, entries: &mut Vec<&'a str>) -> Result<(), ContactError> {
    let source = source.trim_matches([' ', '\t']);
    if source.is_empty() {
        return Err(ContactError::Invalid);
    }
    if entries.len() == MAX_ENTRIES {
        return Err(ContactError::Capacity);
    }
    entries.push(source);
    Ok(())
}

fn parts(source: &str) -> Result<(&str, &str, &str), ContactError> {
    let mut state = Framing::Plain;
    for (offset, character) in source.char_indices() {
        state = match (state, character) {
            (Framing::Plain, '"') => Framing::Quoted,
            (Framing::Plain, '<') => {
                let end = source[offset..].find('>').ok_or(ContactError::Invalid)? + offset;
                return Ok((
                    &source[..=end],
                    &source[offset + 1..end],
                    &source[end + 1..],
                ));
            }
            (Framing::Plain, _) => Framing::Plain,
            (Framing::Quoted, '\\') => Framing::Escaped,
            (Framing::Quoted, '"') => Framing::Plain,
            (Framing::Quoted, _) | (Framing::Escaped, _) => Framing::Quoted,
            (Framing::Address, _) => return Err(ContactError::Invalid),
        };
    }
    let end = source.find(';').unwrap_or(source.len());
    let uri = source[..end].trim_matches([' ', '\t']);
    Ok((uri, uri, &source[end..]))
}

fn contact(source: &str, global: Option<u32>) -> Result<RegistrationContact, ContactError> {
    let (prefix, raw_uri, parameters) = parts(source)?;
    if raw_uri.len() > 2048 {
        return Err(ContactError::Capacity);
    }
    identity::validate(raw_uri)?;
    Contact::parse(prefix).map_err(|_| ContactError::Invalid)?;
    let uri = Uri::try_from(raw_uri).map_err(|_| ContactError::Invalid)?;
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
    let mut expires = None;
    let mut preference = None;
    let parameters = parameters.trim_matches([' ', '\t']);
    if !parameters.is_empty() {
        let mut entries = Vec::new();
        split(
            parameters.strip_prefix(';').ok_or(ContactError::Invalid)?,
            ';',
            &mut entries,
        )?;
        for entry in entries {
            let (name, value) = match entry.split_once('=') {
                Some((name, value)) => (name.trim(), Some(value.trim())),
                None => (entry, None),
            };
            if name.is_empty() || !name.bytes().all(token) {
                return Err(ContactError::Invalid);
            }
            if name.eq_ignore_ascii_case("expires") {
                if expires.is_some() {
                    return Err(ContactError::Invalid);
                }
                expires = Some(decimal(value.ok_or(ContactError::Invalid)?)?);
            } else if name.eq_ignore_ascii_case("q") {
                if preference.is_some() {
                    return Err(ContactError::Invalid);
                }
                preference = Some(quality(value.ok_or(ContactError::Invalid)?)?);
            } else if value.is_some_and(|value| !valid_value(value)) {
                return Err(ContactError::Invalid);
            }
        }
    }
    Ok(RegistrationContact {
        uri,
        source: raw_uri.into(),
        expires: expires.or(global),
        preference: preference.unwrap_or(1000),
    })
}

fn decimal(source: &str) -> Result<u32, ContactError> {
    if source.is_empty() || !source.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ContactError::Invalid);
    }
    source.parse().map_err(|_| ContactError::Invalid)
}

fn quality(source: &str) -> Result<u16, ContactError> {
    let (whole, fraction) = source.split_once('.').unwrap_or((source, ""));
    if !matches!(whole, "0" | "1")
        || fraction.len() > 3
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(ContactError::Invalid);
    }
    let mut fraction_value = 0u16;
    let mut scale = 100u16;
    for byte in fraction.bytes() {
        fraction_value += u16::from(byte - b'0') * scale;
        scale /= 10;
    }
    if whole == "1" {
        if fraction_value != 0 {
            return Err(ContactError::Invalid);
        }
        Ok(1000)
    } else {
        Ok(fraction_value)
    }
}

fn token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-.!%*_+`'~".contains(&byte)
}

fn valid_value(source: &str) -> bool {
    if let Some(quoted) = source.strip_prefix('"') {
        let mut escaped = false;
        for (offset, character) in quoted.char_indices() {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                return offset + 2 == source.len();
            }
        }
        false
    } else {
        !source.is_empty()
            && source
                .bytes()
                .all(|byte| token(byte) || b"[]:".contains(&byte))
    }
}
