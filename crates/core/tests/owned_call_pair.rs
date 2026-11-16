use rusterisk_core::{
    Call, CallEvent, CallPairDecision, CallPairError, CallPairState, OwnedCallPair,
};

fn participant(generation: u64, bridge: u64) -> Result<Call, String> {
    Call::outgoing(generation)
        .apply(CallEvent::Answer)
        .call()
        .join(bridge)
        .map_err(|error| format!("{error:?}"))
}

#[test]
fn only_distinct_answered_participants_in_one_bridge_can_own_the_pair() -> Result<(), String> {
    for bridge in [0, 40, u64::MAX] {
        let left = participant(12, bridge)?;
        let right = participant(14, bridge)?;
        let pair = OwnedCallPair::new(left, right).map_err(|error| format!("{error:?}"))?;
        assert_eq!(pair.bridge(), bridge);
        assert_eq!(pair.generations(), [12, 14]);
        assert_eq!(pair.state(), CallPairState::Linked);
        assert_eq!(
            OwnedCallPair::new(left, left),
            Err(CallPairError::Participants)
        );
        for rejected in [
            Call::outgoing(13),
            Call::outgoing(13).apply(CallEvent::Ring).call(),
            participant(13, bridge)?.apply(CallEvent::Hangup).call(),
        ] {
            assert_eq!(
                OwnedCallPair::new(left, rejected),
                Err(CallPairError::NotAnswered)
            );
            assert_eq!(
                OwnedCallPair::new(rejected, right),
                Err(CallPairError::NotAnswered)
            );
        }
        let detached = right.leave(bridge).map_err(|error| format!("{error:?}"))?;
        assert_eq!(
            OwnedCallPair::new(left, detached),
            Err(CallPairError::Bridge)
        );
        assert_eq!(
            OwnedCallPair::new(detached, left),
            Err(CallPairError::Bridge)
        );
        assert_eq!(
            OwnedCallPair::new(left, participant(14, bridge.wrapping_add(1))?),
            Err(CallPairError::Bridge),
        );
    }
    Ok(())
}

#[test]
fn only_a_current_departure_releases_the_current_peer_and_bridge_once() -> Result<(), String> {
    let left = participant(12, 40)?;
    let right = participant(14, 40)?;
    for (departing, peer) in [(left, right), (right, left)] {
        let mut pair = OwnedCallPair::new(left, right).map_err(|error| format!("{error:?}"))?;
        for (bridge, generation) in [(41, departing.generation()), (40, 11), (40, 13), (40, 99)] {
            assert_eq!(
                pair.participant_ended(bridge, generation, Some(peer)),
                CallPairDecision::Ignore
            );
            assert_eq!(pair.state(), CallPairState::Linked);
        }
        assert_eq!(
            pair.participant_ended(40, departing.generation(), Some(peer)),
            CallPairDecision::EndPeer {
                peer: peer.generation(),
                release: 40
            },
        );
        assert_eq!(pair.state(), CallPairState::Released);
        for generation in [12, 14] {
            assert_eq!(
                pair.participant_ended(40, generation, Some(peer)),
                CallPairDecision::Ignore
            );
        }
        assert_eq!(pair.retire(40), CallPairDecision::Ignore);
    }
    Ok(())
}

#[test]
fn a_moved_replaced_or_ended_peer_is_not_ended_by_the_retired_pair() -> Result<(), String> {
    let left = participant(12, 40)?;
    let right = participant(14, 40)?;
    for (departing, peer) in [(left, right), (right, left)] {
        let moved = peer
            .leave(40)
            .and_then(|call| call.join(41))
            .map_err(|error| format!("{error:?}"))?;
        for current in [
            Some(moved),
            Some(participant(peer.generation().wrapping_add(1), 40)?),
            Some(peer.apply(CallEvent::Hangup).call()),
            None,
        ] {
            let mut pair = OwnedCallPair::new(left, right).map_err(|error| format!("{error:?}"))?;
            assert_eq!(
                pair.participant_ended(40, departing.generation(), current),
                CallPairDecision::ReleaseBridge { release: 40 },
            );
            assert_eq!(pair.state(), CallPairState::Released);
            assert_eq!(
                pair.participant_ended(40, peer.generation(), Some(departing)),
                CallPairDecision::Ignore
            );
        }
        let mut pair = OwnedCallPair::new(left, right).map_err(|error| format!("{error:?}"))?;
        assert_eq!(
            pair.participant_ended(41, departing.generation(), Some(peer)),
            CallPairDecision::Ignore
        );
        assert_eq!(pair.state(), CallPairState::Linked);
    }
    Ok(())
}

#[test]
fn explicit_retirement_releases_only_the_current_bridge_once() -> Result<(), String> {
    let left = participant(12, 40)?;
    let right = participant(14, 40)?;
    let mut pair = OwnedCallPair::new(left, right).map_err(|error| format!("{error:?}"))?;
    assert_eq!(pair.retire(41), CallPairDecision::Ignore);
    assert_eq!(pair.state(), CallPairState::Linked);
    assert_eq!(
        pair.retire(40),
        CallPairDecision::ReleaseBridge { release: 40 }
    );
    assert_eq!(pair.state(), CallPairState::Released);
    assert_eq!(pair.retire(40), CallPairDecision::Ignore);
    for (departing, peer) in [(left, right), (right, left)] {
        assert_eq!(
            pair.participant_ended(40, departing.generation(), Some(peer)),
            CallPairDecision::Ignore
        );
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Observation {
    Left,
    Right,
    MovedPeer,
    ForeignBridge,
    ForeignGeneration,
    Retire,
}

#[test]
fn short_event_sequences_release_once_and_preserve_unrelated_generations() -> Result<(), String> {
    let observations = [
        Observation::Left,
        Observation::Right,
        Observation::MovedPeer,
        Observation::ForeignBridge,
        Observation::ForeignGeneration,
        Observation::Retire,
    ];
    for generation in [0_u64, 1, u64::MAX] {
        for bridge in [0_u64, 1, u64::MAX] {
            let left = participant(generation, bridge)?;
            let right = participant(generation.wrapping_add(1), bridge)?;
            let moved = right
                .leave(bridge)
                .and_then(|call| call.join(bridge.wrapping_add(1)))
                .map_err(|error| format!("{error:?}"))?;
            for mut sequence in 0..observations.len().pow(3) {
                let mut pair =
                    OwnedCallPair::new(left, right).map_err(|error| format!("{error:?}"))?;
                let mut released = false;
                let mut count = 0;
                for _ in 0..3 {
                    let observation = observations[sequence % observations.len()];
                    sequence /= observations.len();
                    let expected = match observation {
                        Observation::Left => Some(CallPairDecision::EndPeer {
                            peer: right.generation(),
                            release: bridge,
                        }),
                        Observation::Right => Some(CallPairDecision::EndPeer {
                            peer: left.generation(),
                            release: bridge,
                        }),
                        Observation::MovedPeer | Observation::Retire => {
                            Some(CallPairDecision::ReleaseBridge { release: bridge })
                        }
                        Observation::ForeignBridge | Observation::ForeignGeneration => None,
                    };
                    let decision = match observation {
                        Observation::Left => {
                            pair.participant_ended(bridge, left.generation(), Some(right))
                        }
                        Observation::Right => {
                            pair.participant_ended(bridge, right.generation(), Some(left))
                        }
                        Observation::MovedPeer => {
                            pair.participant_ended(bridge, left.generation(), Some(moved))
                        }
                        Observation::ForeignBridge => pair.participant_ended(
                            bridge.wrapping_add(1),
                            left.generation(),
                            Some(right),
                        ),
                        Observation::ForeignGeneration => {
                            pair.participant_ended(bridge, generation.wrapping_add(2), Some(right))
                        }
                        Observation::Retire => pair.retire(bridge),
                    };
                    if let Some(expected) = expected.filter(|_| !released) {
                        assert_eq!(decision, expected);
                        released = true;
                        count += 1;
                    } else {
                        assert_eq!(decision, CallPairDecision::Ignore);
                    }
                    assert_eq!(
                        pair.state(),
                        if released {
                            CallPairState::Released
                        } else {
                            CallPairState::Linked
                        }
                    );
                    assert_eq!(pair.bridge(), bridge);
                    assert_eq!(pair.generations(), [left.generation(), right.generation()]);
                }
                assert_eq!(count, usize::from(released));
            }
        }
    }
    Ok(())
}
