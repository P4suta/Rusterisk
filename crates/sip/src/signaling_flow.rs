use crate::SipError;
use std::{net::SocketAddr, num::NonZeroU64};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// An engine-scoped identity for one reliable signaling connection.
/// Callers must use a fresh value for each connection while the engine exists.
pub struct StreamFlowId(NonZeroU64);

impl StreamFlowId {
    pub fn new(value: u64) -> Result<Self, SipError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(SipError::InvalidPacket)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SignalingRoute {
    pub(crate) peer: SocketAddr,
    pub(crate) flow: Option<StreamFlowId>,
}

impl SignalingRoute {
    pub(crate) const fn legacy(peer: SocketAddr) -> Self {
        Self { peer, flow: None }
    }

    pub(crate) const fn stream(peer: SocketAddr, flow: StreamFlowId) -> Self {
        Self {
            peer,
            flow: Some(flow),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Disconnection {
    Legacy(SocketAddr),
    Stream(StreamFlowId),
}

impl Disconnection {
    pub(crate) fn matches(self, route: SignalingRoute) -> bool {
        match self {
            Self::Legacy(peer) => route == SignalingRoute::legacy(peer),
            Self::Stream(flow) => route.flow == Some(flow),
        }
    }
}
