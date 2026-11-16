use rusterisk_api::ChannelState;
use rusterisk_server::{Action, ControlError, Effect, Milestone, Signal};
use rusterisk_sip::{ReferFailure, ReferProgress};
#[path = "transfer_control/support.rs"]
mod support;
use support::Pair;
#[path = "transfer_control/lifetime.rs"]
mod lifetime;

#[test]
fn a_blind_transfer_uses_starting_authority_and_retires_only_the_transferor(
) -> Result<(), ControlError> {
    let mut pair = Pair::new()?;
    assert_eq!(
        pair.control.channel(&pair.source.id)?.dialplan.context,
        "routed"
    );
    let refer = pair.request("200")?;
    let reference = refer.key();
    let transferred = pair.control.signal_at(
        Signal::BlindTransferRequested {
            key: pair.source.clone(),
            refer,
        },
        "transfer",
        10,
    )?;
    let token = transferred
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
                assert_eq!(source, &pair.source);
                assert_eq!(*refer, reference);
                Some(*token)
            } else {
                None
            }
        })
        .ok_or(ControlError::Invalid("Missing transfer acceptance"))?;
    assert!(transferred.effects.iter().any(|effect| matches!(effect, Effect::DialBridge { parent, generation, peer: None } if parent == &pair.source && *generation == pair.bridge)));
    assert!(transferred
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::Release { key } if key == &pair.source)));
    assert!(!transferred
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::Release { key } if key == &pair.survivor)));
    assert!(pair.control.channel(&pair.source.id).is_err());
    assert_eq!(
        pair.control.channel(&pair.survivor.id)?.state,
        ChannelState::Up
    );
    let target = transferred
        .effects
        .iter()
        .find_map(|effect| {
            if let Effect::Start { key, endpoint, .. } = effect {
                assert_eq!(endpoint, "PJSIP/charlie");
                Some(key.clone())
            } else {
                None
            }
        })
        .ok_or(ControlError::Invalid("Missing transferred target"))?;
    assert!(transferred.effects.iter().any(|effect| matches!(effect, Effect::ReferProgress { source, refer, token: active, progress: ReferProgress::HandedOff } if source == &pair.source && *refer == reference && *active == token)));
    let old_end = pair.control.signal_at(
        Signal::Terminated {
            key: pair.source.clone(),
            cause: 16,
        },
        "old end",
        11,
    )?;
    assert!(!old_end.effects.iter().any(|effect| matches!(effect, Effect::Release { key } if key == &pair.survivor || key == &target)));
    assert_eq!(
        pair.control.channel(&pair.survivor.id)?.state,
        ChannelState::Up
    );
    let ringing = pair.control.signal_at(
        Signal::Milestone {
            key: target.clone(),
            milestone: Milestone::Ringing,
        },
        "ringing",
        12,
    )?;
    assert!(ringing.effects.iter().any(|effect| matches!(effect, Effect::ReferProgress { token: active, progress: ReferProgress::Ringing, .. } if *active == token)));
    let answered = pair.control.signal_at(
        Signal::Milestone {
            key: target,
            milestone: Milestone::Answered,
        },
        "answered",
        13,
    )?;
    assert!(answered.effects.iter().any(|effect| matches!(effect, Effect::ReferProgress { source, refer, token: active, progress: ReferProgress::Answered } if source == &pair.source && *refer == reference && *active == token)));
    assert!(answered.effects.iter().any(|effect| matches!(effect, Effect::DialBridge { parent, generation, peer: Some(_) } if parent == &pair.survivor && *generation != pair.bridge)));
    Ok(())
}

#[test]
fn missing_targets_leave_the_live_dial_and_its_cleanup_ownership_intact() -> Result<(), ControlError>
{
    let mut pair = Pair::new()?;
    let refer = pair.request("999")?;
    let failed = pair.control.signal_at(
        Signal::BlindTransferRequested {
            key: pair.source.clone(),
            refer,
        },
        "missing",
        10,
    )?;
    assert!(failed.effects.iter().any(|effect| matches!(
        effect,
        Effect::ReferProgress {
            progress: ReferProgress::Failed(ReferFailure::NotFound),
            ..
        }
    )));
    assert!(!failed.effects.iter().any(|effect| matches!(
        effect,
        Effect::DialBridge { .. } | Effect::Release { .. } | Effect::Start { .. }
    )));
    assert_eq!(
        pair.control.channel(&pair.source.id)?.state,
        ChannelState::Up
    );
    assert_eq!(
        pair.control.channel(&pair.survivor.id)?.state,
        ChannelState::Up
    );
    let ended = pair
        .control
        .apply(Action::Hangup(pair.source.id.clone()), "hangup", 11)?;
    assert!(ended
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::Release { key } if key == &pair.survivor)));
    assert!(pair.control.channel(&pair.survivor.id).is_err());
    Ok(())
}

#[test]
fn stale_sources_and_authority_from_another_endpoint_cannot_detach_the_pair(
) -> Result<(), ControlError> {
    for wrong_generation in [true, false] {
        let mut pair = Pair::new()?;
        let refer = pair.request("200")?;
        let key = if wrong_generation {
            let mut key = pair.source.clone();
            key.generation += 1;
            key
        } else {
            pair.survivor.clone()
        };
        let rejected =
            pair.control
                .signal_at(Signal::BlindTransferRequested { key, refer }, "stale", 10)?;
        assert!(rejected
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::RejectRefer { .. })));
        assert!(!rejected.effects.iter().any(|effect| matches!(
            effect,
            Effect::DialBridge { .. } | Effect::Release { .. } | Effect::Start { .. }
        )));
        assert_eq!(
            pair.control.channel(&pair.source.id)?.state,
            ChannelState::Up
        );
        assert_eq!(
            pair.control.channel(&pair.survivor.id)?.state,
            ChannelState::Up
        );
        let ended = pair
            .control
            .apply(Action::Hangup(pair.source.id.clone()), "hangup", 11)?;
        assert!(ended
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Release { key } if key == &pair.survivor)));
    }
    Ok(())
}
