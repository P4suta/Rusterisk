use super::Header;
use crate::digest::{hexadecimal, text, Algorithm, CredentialEncoding, DigestError};
use std::borrow::Cow;

#[derive(Clone, Copy)]
pub(super) enum UsernameHash {
    Plain,
    Hashed,
}

pub(super) enum Username<'a> {
    Plain(Cow<'a, str>),
    Hashed(&'a str),
}

impl UsernameHash {
    pub(super) fn parse(value: Option<&str>) -> Result<Self, DigestError> {
        match value {
            None => Ok(Self::Plain),
            Some(value) if value.eq_ignore_ascii_case("false") => Ok(Self::Plain),
            Some(value) if value.eq_ignore_ascii_case("true") => Ok(Self::Hashed),
            Some(_) => Err(DigestError::Invalid),
        }
    }

    pub(super) fn render(
        self,
        username: &str,
        realm: &str,
        algorithm: Algorithm,
        encoding: CredentialEncoding,
    ) -> String {
        match self {
            Self::Hashed => format!(
                "username={}, userhash=true",
                quoted(&algorithm.username_hash(username, realm).hex())
            ),
            Self::Plain => {
                if username.is_ascii() || matches!(encoding, CredentialEncoding::Unspecified) {
                    format!("username={}", quoted(username))
                } else {
                    let mut output = String::from("username*=UTF-8''");
                    const HEX: &[u8; 16] = b"0123456789ABCDEF";
                    for byte in username.bytes() {
                        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
                        {
                            output.push(char::from(byte));
                        } else {
                            output.push('%');
                            output.push(char::from(HEX[usize::from(byte >> 4)]));
                            output.push(char::from(HEX[usize::from(byte & 0x0f)]));
                        }
                    }
                    output
                }
            }
        }
    }
}

impl<'a> Username<'a> {
    pub(super) fn parse(header: &'a Header, algorithm: Algorithm) -> Result<Self, DigestError> {
        let hash = UsernameHash::parse(header.literal("userhash")?)?;
        match (
            header.quoted("username")?,
            header.literal("username*")?,
            hash,
        ) {
            (Some(value), None, UsernameHash::Plain) => Ok(Self::Plain(Cow::Borrowed(value))),
            (Some(value), None, UsernameHash::Hashed)
                if value.len() == algorithm.hex_length()
                    && value.bytes().all(super::lower_hexadecimal) =>
            {
                Ok(Self::Hashed(value))
            }
            (None, Some(value), UsernameHash::Plain) => {
                Ok(Self::Plain(Cow::Owned(extended(value)?)))
            }
            (Some(_), Some(_), UsernameHash::Plain | UsernameHash::Hashed)
            | (None, None, UsernameHash::Plain | UsernameHash::Hashed)
            | (Some(_), None, UsernameHash::Hashed)
            | (None, Some(_), UsernameHash::Hashed) => Err(DigestError::Invalid),
        }
    }

    pub(super) fn value(&self) -> &str {
        match self {
            Self::Plain(value) => value,
            Self::Hashed(value) => value,
        }
    }

    pub(super) fn hash(&self) -> UsernameHash {
        match self {
            Self::Plain(_) => UsernameHash::Plain,
            Self::Hashed(_) => UsernameHash::Hashed,
        }
    }

    pub(super) fn matches(
        &self,
        username: &str,
        realm: &str,
        algorithm: Algorithm,
        encoding: CredentialEncoding,
    ) -> Result<bool, DigestError> {
        let prepared = encoding.username(username)?;
        match self {
            Self::Plain(value) => Ok(encoding.username(value)? == prepared),
            Self::Hashed(value) => Ok(algorithm.username_hash(&prepared, realm).matches(value)),
        }
    }
}

pub(super) fn quoted(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        if matches!(character, '"' | '\\') {
            output.push('\\');
        }
        output.push(character);
    }
    output.push('"');
    output
}

pub(super) fn attr(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
        )
}

fn extended(source: &str) -> Result<String, DigestError> {
    let (charset, rest) = source.split_once('\'').ok_or(DigestError::Invalid)?;
    let (language, value) = rest.split_once('\'').ok_or(DigestError::Invalid)?;
    if !language.is_empty()
        && (language.len() > 255
            || language.split('-').any(|part| {
                part.is_empty()
                    || part.len() > 8
                    || !part.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
            || !language
                .split('-')
                .next()
                .is_some_and(|part| part.bytes().all(|byte| byte.is_ascii_alphabetic())))
    {
        return Err(DigestError::Invalid);
    }
    let mut bytes = Vec::with_capacity(value.len().min(256));
    let mut source = value.as_bytes();
    while let Some((&byte, rest)) = source.split_first() {
        let (decoded, rest) = if byte == b'%' {
            let [high, low, rest @ ..] = rest else {
                return Err(DigestError::Invalid);
            };
            let high = hexadecimal(*high).ok_or(DigestError::Invalid)?;
            let low = hexadecimal(*low).ok_or(DigestError::Invalid)?;
            (high * 16 + low, rest)
        } else if attr(byte) {
            (byte, rest)
        } else {
            return Err(DigestError::Invalid);
        };
        if bytes.len() == 256 {
            return Err(DigestError::Invalid);
        }
        bytes.push(decoded);
        source = rest;
    }
    let value = if charset.eq_ignore_ascii_case("UTF-8") {
        String::from_utf8(bytes).map_err(|_| DigestError::Invalid)?
    } else if charset.eq_ignore_ascii_case("ISO-8859-1") {
        bytes.into_iter().map(char::from).collect()
    } else {
        return Err(DigestError::Unsupported);
    };
    if !text(&value, 256) {
        return Err(DigestError::Invalid);
    }
    Ok(value)
}
