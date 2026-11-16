use super::{Credentials, DigestError};
use precis_profiles::{precis_core::profile::Profile, OpaqueString, UsernameCasePreserved};
use std::borrow::Cow;

#[derive(Clone, Copy, Default)]
pub(crate) enum CredentialEncoding {
    #[default]
    Unspecified,
    Utf8,
}

pub(super) struct PreparedCredentials<'a> {
    username: Cow<'a, str>,
    password: Cow<'a, str>,
    realm: &'a str,
}

enum Field {
    Username,
    Password,
}

fn username(source: &str) -> Result<Cow<'_, str>, DigestError> {
    if !source.contains(' ') {
        return UsernameCasePreserved::new()
            .enforce(source)
            .map_err(|_| DigestError::Invalid);
    }
    if source.starts_with(' ') || source.ends_with(' ') {
        return Err(DigestError::Invalid);
    }
    let mut output = String::with_capacity(source.len());
    for (index, part) in source.split(' ').enumerate() {
        if index > 0 {
            output.push(' ');
        }
        if !part.is_empty() {
            output.push_str(
                &UsernameCasePreserved::new()
                    .enforce(part)
                    .map_err(|_| DigestError::Invalid)?,
            );
        }
    }
    Ok(Cow::Owned(output))
}

fn stable(source: &str, field: Field, limit: usize) -> Result<Cow<'_, str>, DigestError> {
    if source.len() > limit {
        return Err(DigestError::Invalid);
    }
    let mut value = Cow::Borrowed(source);
    for _ in 0..4 {
        let prepared = match field {
            Field::Username => username(&value)?,
            Field::Password => OpaqueString::new()
                .enforce(value.as_ref())
                .map_err(|_| DigestError::Invalid)?,
        };
        if prepared.len() > limit {
            return Err(DigestError::Invalid);
        }
        if prepared == value {
            return Ok(value);
        }
        value = Cow::Owned(prepared.into_owned());
    }
    Err(DigestError::Invalid)
}

impl CredentialEncoding {
    pub(super) fn username(self, value: &str) -> Result<Cow<'_, str>, DigestError> {
        match self {
            Self::Unspecified => Ok(Cow::Borrowed(value)),
            Self::Utf8 => stable(value, Field::Username, 256),
        }
    }

    pub(super) fn parse(value: Option<&str>) -> Result<Self, DigestError> {
        match value {
            None => Ok(Self::Unspecified),
            Some(value) if value.eq_ignore_ascii_case("UTF-8") => Ok(Self::Utf8),
            Some(_) => Err(DigestError::Unsupported),
        }
    }

    pub(super) fn prepare<'a>(
        self,
        credentials: &Credentials<'a>,
    ) -> Result<PreparedCredentials<'a>, DigestError> {
        let (username, password) = match self {
            Self::Unspecified => (
                Cow::Borrowed(credentials.username),
                Cow::Borrowed(credentials.password),
            ),
            Self::Utf8 => (
                self.username(credentials.username)?,
                stable(credentials.password, Field::Password, 4096)?,
            ),
        };
        Ok(PreparedCredentials {
            username,
            password,
            realm: credentials.realm,
        })
    }
}

impl PreparedCredentials<'_> {
    pub(super) fn credentials(&self) -> Result<Credentials<'_>, DigestError> {
        Credentials::new(&self.username, &self.password, self.realm)
    }
}
