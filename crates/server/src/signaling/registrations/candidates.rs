use super::peer_waits::{Admission, PeerWaits};
use crate::signaling::resolver::ResolvedTarget;
use crate::ControlError;
use std::net::SocketAddr;

pub(super) struct CandidateCursor {
    resolved: ResolvedTarget,
    next: usize,
}

pub(super) enum Selection {
    Peer(SocketAddr),
    Wait { at: u64 },
    Exhausted,
}

impl CandidateCursor {
    pub(super) const fn new(resolved: ResolvedTarget) -> Self {
        Self { resolved, next: 0 }
    }

    pub(super) fn select(
        &mut self,
        waits: &mut PeerWaits,
        ipv4: bool,
        now: u64,
    ) -> Result<Selection, ControlError> {
        let mut earliest = None;
        for (index, peer) in self.resolved.peers().enumerate().skip(self.next) {
            if peer.is_ipv4() != ipv4 {
                continue;
            }
            match waits.reserve(peer, now)? {
                Admission::Ready => {
                    self.next = index + 1;
                    return Ok(Selection::Peer(peer));
                }
                Admission::Wait { at } => {
                    earliest = Some(earliest.map_or(at, |previous: u64| previous.min(at)));
                }
            }
        }
        if let Some(at) = earliest {
            return Ok(Selection::Wait { at });
        }
        let mut earliest = None;
        for peer in self.resolved.peers().filter(|peer| peer.is_ipv4() == ipv4) {
            let Some(at) = waits.deadline(peer, now) else {
                return Ok(Selection::Exhausted);
            };
            earliest = Some(earliest.map_or(at, |previous: u64| previous.min(at)));
        }
        match earliest {
            Some(at) => {
                self.next = 0;
                Ok(Selection::Wait { at })
            }
            None => Ok(Selection::Exhausted),
        }
    }
}
