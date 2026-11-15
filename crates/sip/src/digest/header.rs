use super::{
    lower_hexadecimal, text, Algorithm, Credentials, DigestError, DigestRequest, Nonce, Protection,
    RequestUri,
};

enum AttributeValue {
    Quoted(String),
    Literal(String),
}

impl AttributeValue {
    fn text(&self) -> &str {
        match self {
            Self::Quoted(value) | Self::Literal(value) => value,
        }
    }
}

struct Attribute {
    name: String,
    value: AttributeValue,
}

pub(crate) struct Header {
    attributes: [Option<Attribute>; 16],
}

impl Header {
    pub(crate) fn parse(source: &str) -> Result<Self, DigestError> {
        if source.len() > 4096
            || source
                .chars()
                .any(|character| character.is_control() && character != '\t')
        {
            return Err(DigestError::Invalid);
        }
        let source = whitespace(source);
        let (scheme, rest) = source.split_once([' ', '\t']).ok_or(DigestError::Invalid)?;
        if !scheme.eq_ignore_ascii_case("Digest") {
            return Err(DigestError::Unsupported);
        }
        let mut rest = whitespace(rest);
        let mut attributes: [Option<Attribute>; 16] = std::array::from_fn(|_| None);
        let mut length = 0;
        loop {
            if rest.is_empty() || length == attributes.len() {
                return Err(DigestError::Invalid);
            }
            let end = rest.bytes().take_while(|byte| token(*byte)).count();
            if end == 0 || end > 32 {
                return Err(DigestError::Invalid);
            }
            let name = &rest[..end];
            if attributes
                .iter()
                .flatten()
                .any(|attribute| attribute.name.eq_ignore_ascii_case(name))
            {
                return Err(DigestError::Invalid);
            }
            rest = whitespace(&rest[end..])
                .strip_prefix('=')
                .ok_or(DigestError::Invalid)?;
            let (value, after) = value(whitespace(rest))?;
            attributes[length] = Some(Attribute {
                name: name.into(),
                value,
            });
            length += 1;
            rest = whitespace(after);
            if rest.is_empty() {
                return Ok(Self { attributes });
            }
            rest = whitespace(rest.strip_prefix(',').ok_or(DigestError::Invalid)?);
        }
    }

    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        self.attributes.iter().flatten().find_map(|attribute| {
            attribute
                .name
                .eq_ignore_ascii_case(name)
                .then_some(attribute.value.text())
        })
    }

    fn attribute(&self, name: &str) -> Option<&AttributeValue> {
        self.attributes.iter().flatten().find_map(|attribute| {
            attribute
                .name
                .eq_ignore_ascii_case(name)
                .then_some(&attribute.value)
        })
    }

    fn quoted(&self, name: &str) -> Result<Option<&str>, DigestError> {
        match self.attribute(name) {
            Some(AttributeValue::Quoted(value)) => Ok(Some(value)),
            Some(AttributeValue::Literal(_)) => Err(DigestError::Invalid),
            None => Ok(None),
        }
    }

    fn literal(&self, name: &str) -> Result<Option<&str>, DigestError> {
        match self.attribute(name) {
            Some(AttributeValue::Literal(value)) => Ok(Some(value)),
            Some(AttributeValue::Quoted(_)) => Err(DigestError::Invalid),
            None => Ok(None),
        }
    }

    fn required_quoted(&self, name: &str) -> Result<&str, DigestError> {
        self.quoted(name)?.ok_or(DigestError::Invalid)
    }

    pub(crate) fn authorization(&self) -> Result<Authorization<'_>, DigestError> {
        if self.get("username*").is_some()
            || self
                .literal("userhash")?
                .is_some_and(|value| !value.eq_ignore_ascii_case("false"))
        {
            return Err(DigestError::Unsupported);
        }
        let username = self.required_quoted("username")?;
        let realm = self.required_quoted("realm")?;
        Credentials::new(username, "", realm)?;
        let nonce = Nonce::new(self.required_quoted("nonce")?)?;
        let uri = RequestUri::new(self.required_quoted("uri")?)?;
        let algorithm = Algorithm::parse(self.literal("algorithm")?)?;
        let response = self.required_quoted("response")?;
        let expected = match algorithm {
            Algorithm::Md5 | Algorithm::Md5Session => 32,
            Algorithm::Sha256
            | Algorithm::Sha256Session
            | Algorithm::Sha512256
            | Algorithm::Sha512256Session => 64,
        };
        if response.len() != expected || !response.bytes().all(lower_hexadecimal) {
            return Err(DigestError::Invalid);
        }
        if self
            .quoted("opaque")?
            .is_some_and(|value| !text(value, 256))
        {
            return Err(DigestError::Invalid);
        }
        let protection = Protection::parse(
            self.literal("qop")?,
            self.quoted("cnonce")?,
            self.literal("nc")?,
        )?;
        Ok(Authorization {
            username,
            realm,
            nonce,
            uri,
            algorithm,
            response,
            protection,
        })
    }
}

pub(crate) struct Authorization<'a> {
    username: &'a str,
    realm: &'a str,
    nonce: Nonce<'a>,
    uri: RequestUri<'a>,
    algorithm: Algorithm,
    response: &'a str,
    protection: Protection<'a>,
}

impl Authorization<'_> {
    pub(crate) fn username(&self) -> &str {
        self.username
    }
    pub(crate) fn realm(&self) -> &str {
        self.realm
    }
    pub(crate) fn nonce(&self) -> &str {
        self.nonce.0
    }
    pub(crate) fn uri(&self) -> &str {
        self.uri.0
    }
    pub(crate) fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    pub(crate) fn protection(&self) -> &Protection<'_> {
        &self.protection
    }

    pub(crate) fn count(&self) -> u32 {
        match &self.protection {
            Protection::Legacy => 1,
            Protection::Auth(client) | Protection::Integrity(client) => client.count.get(),
        }
    }

    pub(crate) fn verify(
        &self,
        method: &str,
        body: &[u8],
        password: &str,
    ) -> Result<bool, DigestError> {
        let credentials = Credentials::new(self.username, password, self.realm)?;
        let request = DigestRequest::new(method, self.uri.0, self.nonce.0, body)?;
        Ok(self
            .algorithm
            .calculate(&credentials, &request, &self.protection)?
            .matches(self.response))
    }
}

fn whitespace(source: &str) -> &str {
    source.trim_matches([' ', '\t'])
}

fn token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'.' | b'!' | b'%' | b'*' | b'_' | b'+' | b'`' | b'\'' | b'~'
        )
}

fn value(source: &str) -> Result<(AttributeValue, &str), DigestError> {
    let Some(quoted) = source.strip_prefix('"') else {
        let end = source.bytes().take_while(|byte| token(*byte)).count();
        return if end == 0 || end > 2048 {
            Err(DigestError::Invalid)
        } else {
            Ok((
                AttributeValue::Literal(source[..end].into()),
                &source[end..],
            ))
        };
    };
    let mut decoded = String::new();
    let mut escaped = false;
    for (offset, character) in quoted.char_indices() {
        if escaped {
            if !character.is_ascii() || character.is_control() {
                return Err(DigestError::Invalid);
            }
            decoded.push(character);
            escaped = false;
        } else {
            match character {
                '"' => return Ok((AttributeValue::Quoted(decoded), &quoted[offset + 1..])),
                '\\' => escaped = true,
                character if character.is_control() => return Err(DigestError::Invalid),
                character => decoded.push(character),
            }
        }
        if decoded.len() > 2048 {
            return Err(DigestError::Invalid);
        }
    }
    Err(DigestError::Invalid)
}

mod challenge;
#[cfg(test)]
mod tests;
