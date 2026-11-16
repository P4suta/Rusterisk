use super::support::Pair;
use rusterisk_api::ChannelState;
use rusterisk_core::TransferToken;
use rusterisk_server::{Action, CallKey, ControlError, Effect, Milestone, Outcome, Signal};
use rusterisk_sip::{ReferFailure, ReferProgress};

fn begin(pair: &mut Pair) -> Result<(TransferToken, CallKey), ControlError> {
    let refer = pair.request("200")?;
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
                token,
                progress: ReferProgress::Trying,
                ..
            } = effect
            {
                Some(*token)
            } else {
                None
            }
        })
        .ok_or(ControlError::Invalid("Missing transfer admission"))?;
    let target = transferred
        .effects
        .iter()
        .find_map(|effect| {
            if let Effect::Start { key, .. } = effect {
                Some(key.clone())
            } else {
                None
            }
        })
        .ok_or(ControlError::Invalid("Missing transfer target"))?;
    Ok((token, target))
}

fn final_progress(outcome: &Outcome) -> Vec<(TransferToken, ReferProgress)> {
    outcome
        .effects
        .iter()
        .filter_map(|effect| {
            if let Effect::ReferProgress {
                token,
                progress: progress @ (ReferProgress::Answered | ReferProgress::Failed(_)),
                ..
            } = effect
            {
                Some((*token, *progress))
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn target_failures_finish_once_before_current_dialplan_cleanup() -> Result<(), ControlError> {
    for (cause, expected) in [
        (17, ReferFailure::Busy),
        (19, ReferFailure::Timeout),
        (34, ReferFailure::Unavailable),
    ] {
        let mut pair = Pair::new()?;
        let (token, target) = begin(&mut pair)?;
        let failed = pair.control.signal_at(
            Signal::Terminated {
                key: target.clone(),
                cause,
            },
            "target failure",
            11,
        )?;
        assert_eq!(
            final_progress(&failed),
            [(token, ReferProgress::Failed(expected))]
        );
        assert_eq!(
            pair.control.channel(&pair.survivor.id)?.state,
            ChannelState::Up
        );
        let cleanup = pair.control.tick_dialplans(12, "dialplan cleanup")?;
        assert!(final_progress(&cleanup).is_empty());
        assert!(cleanup
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Release { key } if key == &pair.survivor)));
        assert!(pair.control.channel(&pair.survivor.id).is_err());
        let late = pair.control.signal_at(
            Signal::Milestone {
                key: target,
                milestone: Milestone::Answered,
            },
            "late answer",
            13,
        )?;
        assert!(final_progress(&late).is_empty());
        assert!(!late
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::DialBridge { .. })));
    }
    Ok(())
}

#[test]
fn target_deadlines_report_timeout_before_releasing_the_survivor() -> Result<(), ControlError> {
    let mut pair = Pair::new()?;
    let (token, target) = begin(&mut pair)?;
    let pending = pair.control.tick_dialplans(5009, "before deadline")?;
    assert!(pending.effects.is_empty());
    let expired = pair.control.tick_dialplans(5010, "deadline")?;
    assert_eq!(
        final_progress(&expired),
        [(token, ReferProgress::Failed(ReferFailure::Timeout))]
    );
    for ended in [&target, &pair.survivor] {
        assert!(expired
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Release { key } if key == ended)));
        assert!(pair.control.channel(&ended.id).is_err());
    }
    let repeated = pair.control.tick_dialplans(5011, "expired again")?;
    assert!(final_progress(&repeated).is_empty());
    Ok(())
}

#[test]
fn survivor_cancellation_releases_the_target_and_absorbs_late_answers() -> Result<(), ControlError>
{
    let mut pair = Pair::new()?;
    let (token, target) = begin(&mut pair)?;
    let cancelled =
        pair.control
            .apply(Action::Hangup(pair.survivor.id.clone()), "cancelled", 11)?;
    assert_eq!(
        final_progress(&cancelled),
        [(token, ReferProgress::Failed(ReferFailure::Cancelled))]
    );
    assert!(cancelled
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::Release { key } if key == &target)));
    let late = pair.control.signal_at(
        Signal::Milestone {
            key: target,
            milestone: Milestone::Answered,
        },
        "late answer",
        12,
    )?;
    assert!(final_progress(&late).is_empty());
    assert!(!late
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::DialBridge { .. })));
    let stopped = pair.control.shutdown("shutdown")?;
    assert!(stopped.effects.is_empty());
    Ok(())
}
