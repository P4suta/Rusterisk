use super::ReferFailure;
use rusterisk_config::{Auth, Endpoint};
#[cfg(test)]
#[path = "authority_tests.rs"]
mod tests;

#[derive(Clone)]
enum Principal {
    Authenticated {
        profile: String,
        realm: String,
        username: String,
    },
    ConfiguredPeer,
}

#[derive(Clone)]
pub struct ReferAuthority {
    endpoint: String,
    context: String,
    principal: Principal,
    pub(super) allow_transfer: bool,
    pub(super) blind_progress: bool,
}

impl std::fmt::Debug for ReferAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReferAuthority")
            .finish_non_exhaustive()
    }
}

impl ReferAuthority {
    pub(crate) fn authenticated(endpoint: &Endpoint, auth: &Auth) -> Self {
        Self::new(
            endpoint,
            Principal::Authenticated {
                profile: auth.name.clone(),
                realm: auth.inbound_realm().to_owned(),
                username: auth.username.clone(),
            },
        )
    }

    pub(crate) fn configured_peer(endpoint: &Endpoint) -> Self {
        Self::new(endpoint, Principal::ConfiguredPeer)
    }

    fn new(endpoint: &Endpoint, principal: Principal) -> Self {
        Self {
            endpoint: endpoint.name.clone(),
            context: endpoint.context.clone(),
            principal,
            allow_transfer: endpoint.allow_transfer,
            blind_progress: endpoint.refer_blind_progress,
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub fn context(&self) -> &str {
        &self.context
    }
    pub fn authenticated_identity(&self) -> Option<&str> {
        match &self.principal {
            Principal::Authenticated { username, .. } => Some(username),
            Principal::ConfiguredPeer => None,
        }
    }

    pub fn replacement_authority(&self, consultation: &Self) -> Result<(), ReferFailure> {
        if !self.allow_transfer || self.context != consultation.context {
            return Err(ReferFailure::Forbidden);
        }
        match (&self.principal, &consultation.principal) {
            (
                Principal::Authenticated {
                    profile,
                    realm,
                    username,
                },
                Principal::Authenticated {
                    profile: consultation_profile,
                    realm: consultation_realm,
                    username: consultation_username,
                },
            ) if profile == consultation_profile
                && realm == consultation_realm
                && username == consultation_username =>
            {
                Ok(())
            }
            (Principal::Authenticated { .. }, Principal::Authenticated { .. })
            | (Principal::ConfiguredPeer, _)
            | (_, Principal::ConfiguredPeer) => Err(ReferFailure::Forbidden),
        }
    }
}
