use super::{CallKey, Control, ControlError, Effect, Outcome};
use rusterisk_core::{CallPairDecision, CallState, OwnedCallPair};
#[cfg(test)]
mod tests;

pub(super) struct ActivePair {
    pub state: OwnedCallPair,
    pub members: [CallKey; 2],
}

impl ActivePair {
    fn peer(&self, key: &CallKey) -> Option<&CallKey> {
        if self.members[0] == *key {
            Some(&self.members[1])
        } else if self.members[1] == *key {
            Some(&self.members[0])
        } else {
            None
        }
    }
}

impl Control {
    pub(super) fn paired_peer(&self, key: &CallKey) -> Option<CallKey> {
        self.call_pairs
            .values()
            .find_map(|pair| pair.peer(key).cloned())
    }

    pub(super) fn end_paired_call(
        &mut self,
        key: &CallKey,
        current_bridge: Option<u64>,
        timestamp: &str,
        now: u64,
    ) -> Result<Outcome, ControlError> {
        let Some((bridge, peer)) = self
            .call_pairs
            .iter()
            .find_map(|(bridge, pair)| pair.peer(key).map(|peer| (*bridge, peer.clone())))
        else {
            return Ok(Outcome::empty());
        };
        let current_peer = self
            .channels
            .get(&peer.id)
            .filter(|entry| entry.call.generation() == peer.generation)
            .map(|entry| entry.call);
        let mut pair = self
            .call_pairs
            .remove(&bridge)
            .ok_or(ControlError::Conflict("Paired ownership changed"))?;
        let decision = match current_bridge {
            Some(current) => pair
                .state
                .participant_ended(current, key.generation, current_peer),
            None => CallPairDecision::Ignore,
        };
        let decision = if decision == CallPairDecision::Ignore {
            pair.state.retire(bridge)
        } else {
            decision
        };
        let mut outcome = Outcome::empty();
        match decision {
            CallPairDecision::Ignore => {}
            CallPairDecision::ReleaseBridge { release } => {
                outcome.effects.push(Effect::ReleaseBridge {
                    generation: release,
                });
                if let Some(entry) = self.channels.get_mut(&peer.id).filter(|entry| {
                    entry.call.generation() == peer.generation
                        && entry.call.bridge() == Some(release)
                        && entry.call.state() != CallState::Ended
                }) {
                    entry.call = entry.call.leave(release)?;
                }
            }
            CallPairDecision::EndPeer {
                peer: generation,
                release,
            } => {
                outcome.effects.push(Effect::ReleaseBridge {
                    generation: release,
                });
                if generation != peer.generation {
                    return Err(ControlError::Conflict("Paired generation changed"));
                }
                outcome.append(self.end_at(&peer.id, timestamp, 16, now)?);
            }
        }
        Ok(outcome)
    }
}
