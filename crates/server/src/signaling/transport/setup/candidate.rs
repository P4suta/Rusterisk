use super::{failure, Prepared, Reserved, SetupEvent, SetupKey};
use crate::ControlError;
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot};

pub(super) enum CandidateDecision {
    Connect,
    Reuse,
}

pub(super) enum PreparedReply {
    Candidate(oneshot::Sender<CandidateDecision>),
    Connected(oneshot::Sender<Reserved>),
}

impl PreparedReply {
    pub(super) fn is_closed(&self) -> bool {
        match self {
            Self::Candidate(reply) => reply.is_closed(),
            Self::Connected(reply) => reply.is_closed(),
        }
    }
}

pub(super) async fn inspect(
    sender: &mpsc::Sender<SetupEvent>,
    key: SetupKey,
    peer: SocketAddr,
) -> Result<CandidateDecision, ControlError> {
    let (reply, decision) = oneshot::channel();
    sender
        .send(SetupEvent::Ready(Prepared {
            key,
            peer,
            reply: PreparedReply::Candidate(reply),
        }))
        .await
        .map_err(failure)?;
    decision
        .await
        .map_err(|_| ControlError::Conflict("Signaling candidate admission ended"))
}
