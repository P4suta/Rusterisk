use crate::digest::CredentialEncoding;
use std::net::SocketAddr;

#[derive(Clone, Copy)]
pub(super) struct AuthenticationState {
    pub(super) attempts: u8,
    pub(super) origin: ChallengeState,
    pub(super) proxy: ChallengeState,
}

impl AuthenticationState {
    pub(super) const fn new() -> Self {
        Self {
            attempts: 0,
            origin: ChallengeState::Unchallenged,
            proxy: ChallengeState::Unchallenged,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum ChallengeState {
    Unchallenged,
    Challenged {
        peer: SocketAddr,
        encoding: CredentialEncoding,
    },
}

impl ChallengeState {
    pub(super) fn challenged_by(self, peer: SocketAddr) -> bool {
        match self {
            Self::Unchallenged => false,
            Self::Challenged { peer: previous, .. } => previous == peer,
        }
    }

    pub(super) const fn encoding(self) -> CredentialEncoding {
        match self {
            Self::Unchallenged => CredentialEncoding::Unspecified,
            Self::Challenged { encoding, .. } => encoding,
        }
    }
}
