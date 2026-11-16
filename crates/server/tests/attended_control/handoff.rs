use super::support::{configuration, Fixture};
use rusterisk_api::ChannelState;
use rusterisk_server::{
    CallKey, ControlError, Effect, IncomingCall, IncomingTarget, Outcome, Signal,
};
use rusterisk_sip::{ReferFailure, ReferProgress};

fn handoff(fixture: &mut Fixture) -> Result<(Outcome, u64), ControlError> {
    let (refer, consultation) = fixture.request()?;
    let reference = refer.key();
    let outcome = fixture.control.signal_at(
        Signal::AttendedTransferRequested {
            key: fixture.source.source.clone(),
            refer: refer.into_source(),
            consultation,
        },
        "attended transfer",
        30,
    )?;
    let token = outcome
        .effects
        .iter()
        .find_map(|effect| {
            if let Effect::ReferProgress {
                source,
                refer,
                token,
                progress: ReferProgress::Trying,
            } = effect
            {
                assert_eq!(source, &fixture.source.source);
                assert_eq!(*refer, reference);
                Some(*token)
            } else {
                None
            }
        })
        .ok_or(ControlError::Invalid("Missing transfer acceptance"))?;
    assert_eq!(
        outcome
            .effects
            .iter()
            .filter(|effect| matches!(effect, Effect::ReferProgress { .. }))
            .count(),
        2
    );
    assert!(outcome.effects.iter().any(|effect| matches!(effect, Effect::ReferProgress { source, refer, token: completed, progress: ReferProgress::Answered } if source == &fixture.source.source && *refer == reference && *completed == token)));
    let bridges = outcome
        .effects
        .iter()
        .filter_map(|effect| {
            if let Effect::DialBridge {
                parent,
                generation,
                peer: Some(peer),
            } = effect
            {
                assert_eq!(parent, &fixture.source.peer);
                assert_eq!(peer, &fixture.consultation.peer);
                Some(*generation)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let [bridge] = bridges.as_slice() else {
        return Err(ControlError::Invalid("Expected one surviving pair bridge"));
    };
    assert!(![fixture.source.bridge, fixture.consultation.bridge].contains(bridge));
    Ok((outcome, *bridge))
}

fn released(outcome: &Outcome, key: &CallKey) -> usize {
    outcome
        .effects
        .iter()
        .filter(|effect| matches!(effect, Effect::Release { key: released } if released == key))
        .count()
}

#[test]
fn handoff_joins_the_existing_peers_before_releasing_the_transferors() -> Result<(), ControlError> {
    let mut fixture = Fixture::new()?;
    let before = [
        fixture.control.channel(&fixture.source.peer.id)?.clone(),
        fixture
            .control
            .channel(&fixture.consultation.peer.id)?
            .clone(),
    ];
    let (outcome, bridge) = handoff(&mut fixture)?;
    for old_bridge in [fixture.source.bridge, fixture.consultation.bridge] {
        assert_eq!(outcome.effects.iter().filter(|effect| matches!(effect, Effect::DialBridge { generation, peer: None, .. } if *generation == old_bridge)).count(), 1);
    }
    let joined = outcome.effects.iter().position(|effect| matches!(effect, Effect::DialBridge { generation, peer: Some(_), .. } if *generation == bridge)).ok_or(ControlError::Invalid("Missing surviving bridge"))?;
    let completed = outcome
        .effects
        .iter()
        .position(|effect| {
            matches!(
                effect,
                Effect::ReferProgress {
                    progress: ReferProgress::Answered,
                    ..
                }
            )
        })
        .ok_or(ControlError::Invalid("Missing final transfer progress"))?;
    for source in [&fixture.source.source, &fixture.consultation.source] {
        assert_eq!(released(&outcome, source), 1);
        let ended = outcome
            .effects
            .iter()
            .position(|effect| matches!(effect, Effect::Release { key } if key == source))
            .ok_or(ControlError::Invalid("Missing transferor release"))?;
        assert!(joined < ended && ended < completed);
        assert!(fixture.control.channel(&source.id).is_err());
        let late = fixture.control.signal_at(
            Signal::Terminated {
                key: source.clone(),
                cause: 16,
            },
            "late transferor end",
            31,
        )?;
        assert!(matches!(late.effects.as_slice(), [Effect::Release { key }] if key == source));
    }
    for (peer, snapshot) in [&fixture.source.peer, &fixture.consultation.peer]
        .into_iter()
        .zip(before)
    {
        assert_eq!(released(&outcome, peer), 0);
        assert_eq!(fixture.control.channel(&peer.id)?, &snapshot);
    }
    assert!(!outcome
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::Start { .. } | Effect::Answer { .. })));
    Ok(())
}

#[test]
fn either_surviving_peer_releases_the_current_peer_and_bridge_once() -> Result<(), ControlError> {
    for role in 0..2 {
        let mut fixture = Fixture::new()?;
        let (_, bridge) = handoff(&mut fixture)?;
        let peers = [
            fixture.source.peer.clone(),
            fixture.consultation.peer.clone(),
        ];
        let ended = fixture.control.signal_at(
            Signal::Terminated {
                key: peers[role].clone(),
                cause: 16,
            },
            "peer end",
            31,
        )?;
        for peer in &peers {
            assert_eq!(released(&ended, peer), 1);
            assert!(fixture.control.channel(&peer.id).is_err());
            let repeated = fixture.control.signal_at(
                Signal::Terminated {
                    key: peer.clone(),
                    cause: 16,
                },
                "duplicate end",
                32,
            )?;
            assert!(
                matches!(repeated.effects.as_slice(), [Effect::Release { key }] if key == peer)
            );
        }
        assert_eq!(ended.effects.iter().filter(|effect| matches!(effect, Effect::ReleaseBridge { generation } if *generation == bridge)).count(), 1);
        assert!(fixture.control.shutdown("stop")?.effects.is_empty());
    }
    Ok(())
}

#[test]
fn shutdown_releases_the_surviving_pair_without_releasing_its_retired_bridges(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new()?;
    let (_, bridge) = handoff(&mut fixture)?;
    let stopped = fixture.control.shutdown("stop")?;
    for peer in [&fixture.source.peer, &fixture.consultation.peer] {
        assert_eq!(released(&stopped, peer), 1);
    }
    assert_eq!(stopped.effects.iter().filter(|effect| matches!(effect, Effect::ReleaseBridge { generation } if *generation == bridge)).count(), 1);
    assert!(!stopped
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::DialBridge { .. })));
    assert!(fixture
        .control
        .shutdown("duplicate stop")?
        .effects
        .is_empty());
    Ok(())
}

#[test]
fn mismatched_starting_authentication_and_context_preserve_both_dial_owners(
) -> Result<(), ControlError> {
    for changed in 0..4 {
        let mut config = configuration()?;
        if changed == 0 {
            config
                .endpoints
                .iter_mut()
                .find(|endpoint| endpoint.name == "consultation")
                .ok_or(ControlError::Invalid("Missing consultation endpoint"))?
                .auth = "other".into();
        } else if changed == 3 {
            config
                .endpoints
                .iter_mut()
                .find(|endpoint| endpoint.name == "consultation")
                .ok_or(ControlError::Invalid("Missing consultation endpoint"))?
                .context = "outside".into();
        } else {
            let auth = config
                .auth
                .iter_mut()
                .find(|auth| auth.name == "shared")
                .ok_or(ControlError::Invalid("Missing authentication profile"))?;
            if changed == 1 {
                auth.realm = "House".into();
            } else {
                auth.username = "Phone".into();
            }
        }
        let mut fixture = Fixture::with_consultation(config)?;
        let keys = [
            fixture.source.source.clone(),
            fixture.source.peer.clone(),
            fixture.consultation.source.clone(),
            fixture.consultation.peer.clone(),
        ];
        let before = keys
            .iter()
            .map(|key| fixture.control.channel(&key.id).cloned())
            .collect::<Result<Vec<_>, _>>()?;
        let (refer, consultation) = fixture.request()?;
        let rejected = fixture.control.signal_at(
            Signal::AttendedTransferRequested {
                key: fixture.source.source.clone(),
                refer: refer.into_source(),
                consultation,
            },
            "unauthorized transfer",
            30,
        )?;
        assert!(matches!(
            rejected.effects.as_slice(),
            [Effect::RejectRefer {
                failure: ReferFailure::Forbidden,
                ..
            }]
        ));
        for (key, snapshot) in keys.iter().zip(before) {
            assert_eq!(fixture.control.channel(&key.id)?, &snapshot);
        }
        let ended = fixture.control.signal_at(
            Signal::Terminated {
                key: fixture.source.source.clone(),
                cause: 16,
            },
            "original owner end",
            31,
        )?;
        assert_eq!(released(&ended, &fixture.source.peer), 1);
        assert!(fixture.control.channel(&fixture.source.peer.id).is_err());
        for key in [&fixture.consultation.source, &fixture.consultation.peer] {
            assert_eq!(fixture.control.channel(&key.id)?.state, ChannelState::Up);
        }
        fixture.control.shutdown("stop")?;
    }
    Ok(())
}

#[test]
fn a_stale_source_generation_cannot_start_an_attended_handoff() -> Result<(), ControlError> {
    let mut fixture = Fixture::new()?;
    let (refer, consultation) = fixture.request()?;
    let mut stale = fixture.source.source.clone();
    stale.generation = stale
        .generation
        .checked_add(1)
        .ok_or(ControlError::Capacity)?;
    let rejected = fixture.control.signal_at(
        Signal::AttendedTransferRequested {
            key: stale,
            refer: refer.into_source(),
            consultation,
        },
        "stale source",
        30,
    )?;
    assert!(matches!(
        rejected.effects.as_slice(),
        [Effect::RejectRefer {
            failure: ReferFailure::NotFound,
            ..
        }]
    ));
    for key in [
        &fixture.source.source,
        &fixture.source.peer,
        &fixture.consultation.source,
        &fixture.consultation.peer,
    ] {
        assert_eq!(fixture.control.channel(&key.id)?.state, ChannelState::Up);
    }
    let stopped = fixture.control.shutdown("stop")?;
    for key in [
        &fixture.source.source,
        &fixture.source.peer,
        &fixture.consultation.source,
        &fixture.consultation.peer,
    ] {
        assert_eq!(released(&stopped, key), 1);
    }
    Ok(())
}

#[test]
fn selecting_the_source_pair_preserves_both_original_dial_bridges() -> Result<(), ControlError> {
    let mut fixture = Fixture::new()?;
    let (refer, consultation) = fixture.request_same_pair()?;
    let rejected = fixture.control.signal_at(
        Signal::AttendedTransferRequested {
            key: fixture.source.source.clone(),
            refer: refer.into_source(),
            consultation,
        },
        "overlapping pairs",
        30,
    )?;
    assert!(matches!(
        rejected.effects.as_slice(),
        [Effect::RejectRefer {
            failure: ReferFailure::Declined,
            ..
        }]
    ));
    for key in [
        &fixture.source.source,
        &fixture.source.peer,
        &fixture.consultation.source,
        &fixture.consultation.peer,
    ] {
        assert_eq!(fixture.control.channel(&key.id)?.state, ChannelState::Up);
    }
    let stopped = fixture.control.shutdown("stop")?;
    for old in [fixture.source.bridge, fixture.consultation.bridge] {
        assert_eq!(stopped.effects.iter().filter(|effect| matches!(effect, Effect::DialBridge { generation, peer: None, .. } if *generation == old)).count(), 1);
    }
    Ok(())
}

#[test]
fn a_reused_consultation_identifier_does_not_reuse_its_generation_or_authority(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new()?;
    let (refer, consultation) = fixture.request()?;
    fixture.control.signal_at(
        Signal::Terminated {
            key: fixture.consultation.source.clone(),
            cause: 16,
        },
        "consultation ended",
        25,
    )?;
    let id = fixture.consultation.source.id.clone();
    fixture.control.incoming(
        IncomingCall {
            id: id.clone(),
            endpoint: "consultation".into(),
            caller: "replacement".into(),
            context: "internal".into(),
            extension: "200".into(),
            target: IncomingTarget::Dialplan,
        },
        "reused id",
    )?;
    let replacement = fixture.control.key(&id)?;
    assert_ne!(replacement.generation, consultation.generation());
    let snapshot = fixture.control.channel(&id)?.clone();
    let rejected = fixture.control.signal_at(
        Signal::AttendedTransferRequested {
            key: fixture.source.source.clone(),
            refer: refer.into_source(),
            consultation,
        },
        "stale consultation",
        30,
    )?;
    assert!(matches!(
        rejected.effects.as_slice(),
        [Effect::RejectRefer {
            failure: ReferFailure::NotFound,
            ..
        }]
    ));
    assert_eq!(fixture.control.channel(&id)?, &snapshot);
    for key in [&fixture.source.source, &fixture.source.peer] {
        assert_eq!(fixture.control.channel(&key.id)?.state, ChannelState::Up);
    }
    fixture.control.shutdown("stop")?;
    Ok(())
}
