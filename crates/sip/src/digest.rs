use md5::Md5;
use rsipstack::sip::{uri::Scheme, Method, Uri};
use sha2::{Digest, Sha256, Sha512_256};
use std::num::NonZeroU32;
use subtle::ConstantTimeEq;
mod header;
pub(crate) use header::Header;
#[cfg(test)]
mod challenge_tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DigestError {
    Invalid,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Algorithm {
    Md5,
    Md5Session,
    Sha256,
    Sha256Session,
    Sha512256,
    Sha512256Session,
}

impl Algorithm {
    pub(crate) fn parse(value: Option<&str>) -> Result<Self, DigestError> {
        let value = value.unwrap_or("MD5");
        [
            Self::Md5,
            Self::Md5Session,
            Self::Sha256,
            Self::Sha256Session,
            Self::Sha512256,
            Self::Sha512256Session,
        ]
        .into_iter()
        .find(|algorithm| algorithm.name().eq_ignore_ascii_case(value))
        .ok_or(DigestError::Unsupported)
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Md5Session => "MD5-sess",
            Self::Sha256 => "SHA-256",
            Self::Sha256Session => "SHA-256-sess",
            Self::Sha512256 => "SHA-512-256",
            Self::Sha512256Session => "SHA-512-256-sess",
        }
    }

    fn hash(self, parts: &[&[u8]]) -> Response {
        match self {
            Self::Md5 | Self::Md5Session => {
                let mut digest = Md5::new();
                for part in parts {
                    digest.update(part);
                }
                Response::Md5(digest.finalize().into())
            }
            Self::Sha256 | Self::Sha256Session => {
                let mut digest = Sha256::new();
                for part in parts {
                    digest.update(part);
                }
                Response::Sha256(digest.finalize().into())
            }
            Self::Sha512256 | Self::Sha512256Session => {
                let mut digest = Sha512_256::new();
                for part in parts {
                    digest.update(part);
                }
                Response::Sha512256(digest.finalize().into())
            }
        }
    }

    pub(crate) fn calculate(
        self,
        credentials: &Credentials<'_>,
        request: &DigestRequest<'_>,
        protection: &Protection<'_>,
    ) -> Result<Response, DigestError> {
        let mut identity = self
            .hash(&[
                credentials.username.as_bytes(),
                b":",
                credentials.realm.as_bytes(),
                b":",
                credentials.password.as_bytes(),
            ])
            .hex();
        if matches!(
            self,
            Self::Md5Session | Self::Sha256Session | Self::Sha512256Session
        ) {
            let client = match protection {
                Protection::Legacy => return Err(DigestError::Invalid),
                Protection::Auth(client) | Protection::Integrity(client) => client,
            };
            identity = self
                .hash(&[
                    identity.as_bytes(),
                    b":",
                    request.nonce.0.as_bytes(),
                    b":",
                    client.nonce.0.as_bytes(),
                ])
                .hex();
        }
        let target = match protection {
            Protection::Legacy | Protection::Auth(_) => {
                self.hash(&[request.method.as_bytes(), b":", request.uri.0.as_bytes()])
            }
            Protection::Integrity(_) => {
                let entity = self.hash(&[request.body]).hex();
                self.hash(&[
                    request.method.as_bytes(),
                    b":",
                    request.uri.0.as_bytes(),
                    b":",
                    entity.as_bytes(),
                ])
            }
        }
        .hex();
        match protection {
            Protection::Legacy => Ok(self.hash(&[
                identity.as_bytes(),
                b":",
                request.nonce.0.as_bytes(),
                b":",
                target.as_bytes(),
            ])),
            Protection::Auth(client) | Protection::Integrity(client) => {
                let count = format!("{:08x}", client.count);
                let quality = match protection {
                    Protection::Auth(_) => b"auth".as_slice(),
                    Protection::Integrity(_) => b"auth-int".as_slice(),
                    Protection::Legacy => return Err(DigestError::Invalid),
                };
                Ok(self.hash(&[
                    identity.as_bytes(),
                    b":",
                    request.nonce.0.as_bytes(),
                    b":",
                    count.as_bytes(),
                    b":",
                    client.nonce.0.as_bytes(),
                    b":",
                    quality,
                    b":",
                    target.as_bytes(),
                ]))
            }
        }
    }
}

pub(crate) struct Credentials<'a> {
    username: &'a str,
    password: &'a str,
    realm: &'a str,
}

impl<'a> Credentials<'a> {
    pub(crate) fn new(
        username: &'a str,
        password: &'a str,
        realm: &'a str,
    ) -> Result<Self, DigestError> {
        if !text(username, 256)
            || username.is_empty()
            || username.contains(':')
            || !text(password, 4096)
            || !text(realm, 256)
        {
            return Err(DigestError::Invalid);
        }
        Ok(Self {
            username,
            password,
            realm,
        })
    }
}

struct RequestUri<'a>(&'a str);

impl<'a> RequestUri<'a> {
    fn new(value: &'a str) -> Result<Self, DigestError> {
        if !text(value, 2048)
            || value.bytes().any(|byte| byte.is_ascii_whitespace())
            || value.contains(['"', '\\'])
        {
            return Err(DigestError::Invalid);
        }
        let uri = Uri::try_from(value).map_err(|_| DigestError::Invalid)?;
        if !matches!(uri.scheme, Some(Scheme::Sip | Scheme::Sips)) {
            return Err(DigestError::Unsupported);
        }
        Ok(Self(value))
    }
}

struct Nonce<'a>(&'a str);

impl<'a> Nonce<'a> {
    fn new(value: &'a str) -> Result<Self, DigestError> {
        if value.is_empty() || !text(value, 256) || !value.is_ascii() {
            return Err(DigestError::Invalid);
        }
        Ok(Self(value))
    }
}

pub(crate) struct DigestRequest<'a> {
    method: &'a str,
    uri: RequestUri<'a>,
    nonce: Nonce<'a>,
    body: &'a [u8],
}

impl<'a> DigestRequest<'a> {
    pub(crate) fn new(
        method: &'a str,
        uri: &'a str,
        nonce: &'a str,
        body: &'a [u8],
    ) -> Result<Self, DigestError> {
        if method.len() > 64
            || method.bytes().any(|byte| byte.is_ascii_whitespace())
            || Method::try_from(method).is_err()
            || body.len() > crate::MAX_MESSAGE
        {
            return Err(DigestError::Invalid);
        }
        Ok(Self {
            method,
            uri: RequestUri::new(uri)?,
            nonce: Nonce::new(nonce)?,
            body,
        })
    }
}

pub(crate) struct ClientNonce<'a> {
    nonce: Nonce<'a>,
    count: NonZeroU32,
}

pub(crate) enum Protection<'a> {
    Legacy,
    Auth(ClientNonce<'a>),
    Integrity(ClientNonce<'a>),
}

impl<'a> Protection<'a> {
    pub(crate) fn parse(
        quality: Option<&str>,
        nonce: Option<&'a str>,
        count: Option<&str>,
    ) -> Result<Self, DigestError> {
        let Some(quality) = quality else {
            return if nonce.is_none() && count.is_none() {
                Ok(Self::Legacy)
            } else {
                Err(DigestError::Invalid)
            };
        };
        let integrity = if quality.eq_ignore_ascii_case("auth") {
            false
        } else if quality.eq_ignore_ascii_case("auth-int") {
            true
        } else {
            return Err(DigestError::Unsupported);
        };
        let nonce = Nonce::new(nonce.ok_or(DigestError::Invalid)?)?;
        let count = count.ok_or(DigestError::Invalid)?;
        if count.len() != 8 || !count.bytes().all(lower_hexadecimal) {
            return Err(DigestError::Invalid);
        }
        let count =
            NonZeroU32::new(u32::from_str_radix(count, 16).map_err(|_| DigestError::Invalid)?)
                .ok_or(DigestError::Invalid)?;
        let client = ClientNonce { nonce, count };
        Ok(if integrity {
            Self::Integrity(client)
        } else {
            Self::Auth(client)
        })
    }
}

pub(crate) enum Response {
    Md5([u8; 16]),
    Sha256([u8; 32]),
    Sha512256([u8; 32]),
}

impl Response {
    pub(crate) fn hex(&self) -> String {
        let bytes = match self {
            Self::Md5(bytes) => bytes.as_slice(),
            Self::Sha256(bytes) | Self::Sha512256(bytes) => bytes.as_slice(),
        };
        let mut encoded = String::with_capacity(bytes.len() * 2);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in bytes {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        encoded
    }

    pub(crate) fn matches(&self, encoded: &str) -> bool {
        let expected = match self {
            Self::Md5(bytes) => bytes.as_slice(),
            Self::Sha256(bytes) | Self::Sha512256(bytes) => bytes.as_slice(),
        };
        if encoded.len() != expected.len() * 2 {
            return false;
        }
        let mut actual = [0; 32];
        for (index, pair) in encoded.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let Some(high) = hexadecimal(pair[0]) else {
                return false;
            };
            let Some(low) = hexadecimal(pair[1]) else {
                return false;
            };
            actual[index] = high * 16 + low;
        }
        bool::from(expected.ct_eq(&actual[..expected.len()]))
    }
}

fn hexadecimal(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn lower_hexadecimal(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

fn text(value: &str, maximum: usize) -> bool {
    value.len() <= maximum && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests;
