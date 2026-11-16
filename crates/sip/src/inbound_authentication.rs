use crate::{digest, Engine, Nonce, SipError};
use rsipstack::sip::{HeadersExt, Request, Uri};
use rusterisk_config::{Auth, DigestAlgorithm};
use std::net::SocketAddr;

#[derive(PartialEq, Eq)]
pub(super) struct ChallengeProfile {
    realm: String,
    algorithm: digest::Algorithm,
}

impl ChallengeProfile {
    fn new(realm: &str, algorithm: DigestAlgorithm) -> Self {
        Self {
            realm: realm.into(),
            algorithm: algorithm.into(),
        }
    }

    fn permits(&self, auth: &Auth) -> bool {
        auth.inbound_realm() == self.realm
            && auth
                .supported_algorithms_uas
                .iter()
                .any(|algorithm| digest::Algorithm::from(algorithm) == self.algorithm)
    }
}

impl Engine {
    pub(super) fn authorize(
        &mut self,
        request: &Request,
        peer: SocketAddr,
        now: u64,
    ) -> Result<Option<usize>, SipError> {
        let Some(header) = request.authorization_header() else {
            return Ok(None);
        };
        let Ok(header) = digest::Header::parse(header.value()) else {
            return Ok(None);
        };
        let Ok(authorization) = header.authorization() else {
            return Ok(None);
        };
        let Some(nonce) = self.nonces.get(authorization.nonce()) else {
            return Ok(None);
        };
        if nonce.peer != peer
            || nonce.expires <= now
            || nonce.call_id != request.call_id_header()?.value()
            || Uri::try_from(authorization.uri())? != request.uri
            || authorization.algorithm() != nonce.profile.algorithm
            || authorization.realm() != nonce.profile.realm
            || authorization.count() <= nonce.count
        {
            return Ok(None);
        }
        let mut selected: Option<(usize, &Auth)> = None;
        for (index, endpoint) in self.config.endpoints.iter().enumerate() {
            let Some(auth) = self
                .config
                .auth
                .iter()
                .find(|auth| auth.name == endpoint.auth)
            else {
                continue;
            };
            if !nonce.profile.permits(auth)
                || !matches!(
                    authorization
                        .matches_username(&auth.username, digest::CredentialEncoding::Utf8),
                    Ok(true)
                )
            {
                continue;
            }
            if let Some((_, prior)) = selected {
                if prior.name != auth.name {
                    return Ok(None);
                }
            } else {
                selected = Some((index, auth));
            }
        }
        let Some((endpoint_index, auth)) = selected else {
            return Ok(None);
        };
        let credentials = match digest::Credentials::new(
            &auth.username,
            auth.password.expose(),
            auth.inbound_realm(),
        ) {
            Ok(credentials) => credentials,
            Err(_) => return Ok(None),
        };
        if !matches!(
            authorization.verify_credentials(
                &request.method.to_string(),
                &request.body,
                &credentials,
                digest::CredentialEncoding::Utf8
            ),
            Ok(true)
        ) {
            return Ok(None);
        }
        let endpoint = &self.config.endpoints[endpoint_index];
        if self
            .config
            .identify
            .iter()
            .any(|entry| entry.endpoint == endpoint.name)
            && !self
                .config
                .identify
                .iter()
                .any(|entry| entry.endpoint == endpoint.name && entry.address == peer.ip())
        {
            return Ok(None);
        }
        self.nonces
            .get_mut(authorization.nonce())
            .ok_or(SipError::InvalidPacket)?
            .count = authorization.count();
        Ok(Some(endpoint_index))
    }

    pub(super) fn challenge(
        &mut self,
        request: &Request,
        peer: SocketAddr,
        now: u64,
    ) -> Result<Vec<u8>, SipError> {
        let mut profiles = Vec::with_capacity(3);
        for endpoint in &self.config.endpoints {
            let Some(auth) = self
                .config
                .auth
                .iter()
                .find(|auth| auth.name == endpoint.auth)
            else {
                continue;
            };
            for algorithm in auth.supported_algorithms_uas.iter() {
                let profile = ChallengeProfile::new(auth.inbound_realm(), algorithm);
                if !profiles.contains(&profile) {
                    if profiles.len() == 128 {
                        return self.render_response(request, 503, "Service Unavailable", "", "");
                    }
                    profiles.push(profile);
                }
            }
        }
        if profiles.is_empty() {
            profiles.push(ChallengeProfile::new("asterisk", DigestAlgorithm::Md5));
        }
        if self
            .nonces
            .len()
            .checked_add(profiles.len())
            .is_none_or(|count| count > 128)
        {
            return self.render_response(request, 503, "Service Unavailable", "", "");
        }
        let expires = now.checked_add(60_000).ok_or(SipError::Capacity)?;
        let mut issued = Vec::with_capacity(profiles.len());
        let mut extra = String::new();
        for profile in profiles {
            let nonce = self.token()?;
            let value = digest::Header::server_challenge(&profile.realm, &nonce, profile.algorithm)
                .map_err(|_| SipError::InvalidPacket)?;
            extra.push_str("WWW-Authenticate: ");
            extra.push_str(&value);
            extra.push_str("\r\n");
            if extra.len() > crate::MAX_MESSAGE {
                return self.render_response(request, 503, "Service Unavailable", "", "");
            }
            issued.push((
                nonce,
                Nonce {
                    peer,
                    call_id: request.call_id_header()?.value().into(),
                    expires,
                    count: 0,
                    profile,
                },
            ));
        }
        let reply = match self.render_response(request, 401, "Unauthorized", &extra, "") {
            Ok(reply)
                if std::str::from_utf8(&reply)?
                    .split("\r\n")
                    .skip(1)
                    .take_while(|line| !line.is_empty())
                    .count()
                    <= 64 =>
            {
                reply
            }
            Ok(_) | Err(SipError::Capacity) => {
                return self.render_response(request, 503, "Service Unavailable", "", "")
            }
            Err(error) => return Err(error),
        };
        for (nonce, state) in issued {
            self.nonces.insert(nonce, state);
        }
        Ok(reply)
    }
}
