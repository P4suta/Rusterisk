use super::Header;
use crate::digest::{text, Algorithm, Credentials, DigestError, DigestRequest, Nonce, Protection};
use rusterisk_config::Auth;

enum Quality {
    Auth,
    Integrity,
}

pub(crate) struct Challenge {
    realm: String,
    nonce: String,
    algorithm: Algorithm,
    quality: Quality,
    opaque: Option<String>,
    stale: bool,
}

pub(crate) struct ChallengeChoice<'a> {
    challenge: Challenge,
    credentials: &'a Auth,
}

impl Header {
    pub(crate) fn choose_challenge<'a, 'b>(
        offers: impl IntoIterator<Item = &'a str>,
        credentials: &'b [Auth],
        previous: Option<&str>,
    ) -> Result<Option<ChallengeChoice<'b>>, DigestError> {
        let previous = previous.map(prior_nonce).transpose()?;
        for source in offers.into_iter().take(64) {
            let challenge = match Self::parse(source).and_then(|header| header.challenge()) {
                Ok(challenge) => challenge,
                Err(DigestError::Invalid | DigestError::Unsupported) => continue,
            };
            if previous
                .as_ref()
                .is_some_and(|previous| !challenge.stale() || previous == challenge.nonce())
            {
                continue;
            }
            let selected = credentials
                .iter()
                .find(|credentials| credentials.realm == challenge.realm())
                .or_else(|| {
                    credentials
                        .iter()
                        .find(|credentials| credentials.matches_outbound_realm(challenge.realm()))
                });
            if let Some(credentials) = selected {
                return Ok(Some(ChallengeChoice {
                    challenge,
                    credentials,
                }));
            }
        }
        Ok(None)
    }

    pub(crate) fn challenge(&self) -> Result<Challenge, DigestError> {
        let realm = self.required_quoted("realm")?;
        if !text(realm, 256) {
            return Err(DigestError::Invalid);
        }
        let nonce = self.required_quoted("nonce")?;
        Nonce::new(nonce)?;
        let algorithm = Algorithm::parse(self.literal("algorithm")?)?;
        let quality = quality(self.quoted("qop")?)?;
        let stale = match self.literal("stale")? {
            Some(value) if value.eq_ignore_ascii_case("true") => true,
            Some(value) if value.eq_ignore_ascii_case("false") => false,
            None => false,
            Some(_) => return Err(DigestError::Invalid),
        };
        if self.get("charset").is_some()
            || self
                .literal("userhash")?
                .is_some_and(|value| !value.eq_ignore_ascii_case("false"))
        {
            return Err(DigestError::Unsupported);
        }
        let opaque = self.quoted("opaque")?;
        if opaque.is_some_and(|value| !text(value, 256)) {
            return Err(DigestError::Invalid);
        }
        Ok(Challenge {
            realm: realm.into(),
            nonce: nonce.into(),
            algorithm,
            quality,
            opaque: opaque.map(String::from),
            stale,
        })
    }
}

impl Challenge {
    pub(crate) fn realm(&self) -> &str {
        &self.realm
    }
    pub(crate) fn nonce(&self) -> &str {
        &self.nonce
    }
    pub(crate) fn algorithm(&self) -> Algorithm {
        self.algorithm
    }
    pub(crate) fn quality(&self) -> &'static str {
        match self.quality {
            Quality::Auth => "auth",
            Quality::Integrity => "auth-int",
        }
    }
    pub(crate) fn stale(&self) -> bool {
        self.stale
    }

    pub(crate) fn authorization(
        &self,
        credentials: &Credentials<'_>,
        request: &DigestRequest<'_>,
        client_nonce: &str,
        count: u32,
    ) -> Result<String, DigestError> {
        if credentials.realm != self.realm() || request.nonce.0 != self.nonce() {
            return Err(DigestError::Invalid);
        }
        let counter = format!("{count:08x}");
        let protection =
            Protection::parse(Some(self.quality()), Some(client_nonce), Some(&counter))?;
        let response = self
            .algorithm()
            .calculate(credentials, request, &protection)?
            .hex();
        let mut output = format!(
            "Digest username={}, realm={}, nonce={}, uri={}, response=\"{response}\", algorithm={}, qop={}, cnonce={}, nc={counter}",
            quoted(credentials.username),
            quoted(&self.realm),
            quoted(&self.nonce),
            quoted(request.uri.0),
            self.algorithm().name(),
            self.quality(),
            quoted(client_nonce),
        );
        if let Some(opaque) = &self.opaque {
            output.push_str(", opaque=");
            output.push_str(&quoted(opaque));
        }
        if output.len() > 4096 {
            return Err(DigestError::Invalid);
        }
        Ok(output)
    }
}

impl ChallengeChoice<'_> {
    pub(crate) fn authorization(
        &self,
        method: &str,
        uri: &str,
        body: &[u8],
        client_nonce: &str,
        count: u32,
    ) -> Result<String, DigestError> {
        let credentials = Credentials::new(
            &self.credentials.username,
            self.credentials.password.expose(),
            self.challenge.realm(),
        )?;
        let request = DigestRequest::new(method, uri, self.challenge.nonce(), body)?;
        self.challenge
            .authorization(&credentials, &request, client_nonce, count)
    }
}

fn prior_nonce(source: &str) -> Result<String, DigestError> {
    let header = Header::parse(source)?;
    Ok(header.authorization()?.nonce().into())
}

fn quality(value: Option<&str>) -> Result<Quality, DigestError> {
    let Some(value) = value else {
        return Ok(Quality::Auth);
    };
    let mut selected = None;
    for (index, offered) in value.split(',').enumerate() {
        let offered = offered.trim_matches([' ', '\t']);
        if index == 8 || offered.is_empty() || !offered.bytes().all(super::token) {
            return Err(DigestError::Invalid);
        }
        if offered.eq_ignore_ascii_case("auth-int") {
            selected = Some(Quality::Integrity);
        } else if offered.eq_ignore_ascii_case("auth") && selected.is_none() {
            selected = Some(Quality::Auth);
        }
    }
    selected.ok_or(DigestError::Unsupported)
}

fn quoted(value: &str) -> String {
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
