use super::{
    identity::{quoted, UsernameHash},
    Header,
};
use crate::digest::{
    text, Algorithm, CredentialEncoding, Credentials, DigestError, DigestRequest, Nonce, Protection,
};
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
    encoding: CredentialEncoding,
    username_hash: UsernameHash,
}

pub(crate) struct ChallengeChoice<'a> {
    challenge: Challenge,
    credentials: &'a Auth,
}

impl Header {
    pub(crate) fn server_challenge(
        realm: &str,
        nonce: &str,
        algorithm: Algorithm,
    ) -> Result<String, DigestError> {
        if !text(realm, 256) {
            return Err(DigestError::Invalid);
        }
        Nonce::new(nonce)?;
        Ok(format!(
            "Digest realm={}, nonce={}, algorithm={}, qop=\"auth,auth-int\", charset=UTF-8, userhash=true",
            quoted(realm), quoted(nonce), algorithm.name()
        ))
    }

    pub(crate) fn renew_authorization(
        &self,
        credentials: &[Auth],
        method: &str,
        uri: &str,
        body: &[u8],
        encoding: CredentialEncoding,
    ) -> Result<Option<String>, DigestError> {
        let prior = self.authorization()?;
        let quality = match prior.protection() {
            Protection::Auth(_) => Quality::Auth,
            Protection::Integrity(_) => Quality::Integrity,
            Protection::Legacy => return Err(DigestError::Unsupported),
        };
        let mut exact = None;
        let mut wildcard = None;
        for candidate in credentials {
            if !candidate.matches_outbound_realm(prior.realm())
                || !prior.matches_username(&candidate.username, encoding)?
            {
                continue;
            }
            if candidate.realm == prior.realm() {
                exact = Some(candidate);
                break;
            }
            if wildcard.is_none() {
                wildcard = Some(candidate);
            }
        }
        let selected = exact.or(wildcard);
        let Some(selected) = selected else {
            return Ok(None);
        };
        let challenge = Challenge {
            realm: prior.realm().into(),
            nonce: prior.nonce().into(),
            algorithm: prior.algorithm(),
            quality,
            opaque: self.quoted("opaque")?.map(String::from),
            stale: false,
            encoding,
            username_hash: prior.username.hash(),
        };
        let count = prior.count().checked_add(1).ok_or(DigestError::Invalid)?;
        let credentials = Credentials::new(
            &selected.username,
            selected.password.expose(),
            prior.realm(),
        )?;
        let request = DigestRequest::new(method, uri, prior.nonce(), body)?;
        challenge
            .authorization(
                &credentials,
                &request,
                self.required_quoted("cnonce")?,
                count,
            )
            .map(Some)
    }

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
        let encoding = CredentialEncoding::parse(self.get("charset"))?;
        let username_hash = UsernameHash::parse(self.literal("userhash")?)?;
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
            encoding,
            username_hash,
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
        let prepared = self.encoding.prepare(credentials)?;
        let credentials = prepared.credentials()?;
        let counter = format!("{count:08x}");
        let protection =
            Protection::parse(Some(self.quality()), Some(client_nonce), Some(&counter))?;
        let response = self
            .algorithm()
            .calculate(&credentials, request, &protection)?
            .hex();
        let username = self.username_hash.render(
            credentials.username,
            &self.realm,
            self.algorithm,
            self.encoding,
        );
        let mut output = format!(
            "Digest {username}, realm={}, nonce={}, uri={}, response=\"{response}\", algorithm={}, qop={}, cnonce={}, nc={counter}",
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
    pub(crate) const fn encoding(&self) -> CredentialEncoding {
        self.challenge.encoding
    }

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
