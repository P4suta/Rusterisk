use rusterisk_core::{CallCompletion, CallMilestone, OwnedCallProgress};

const MILESTONES: [CallMilestone; 4] = [
    CallMilestone::Accepted,
    CallMilestone::Stasis,
    CallMilestone::Answered,
    CallMilestone::MediaConnected,
];

#[test]
fn observations_are_independent_monotonic_and_idempotent() {
    for first in MILESTONES {
        let progress = OwnedCallProgress::new().observe(first);
        assert_eq!(progress.observe(first), progress);
        for other in MILESTONES {
            assert_eq!(progress.has_reached(other), first == other);
            let both = progress.observe(other);
            assert!(both.has_reached(first));
            assert!(both.has_reached(other));
            assert_eq!(both.completion(), None);
        }
    }
}

#[test]
fn every_terminal_result_absorbs_late_observations_and_other_results() {
    for mask in 0..16 {
        let mut progress = OwnedCallProgress::new();
        for (index, milestone) in MILESTONES.iter().enumerate() {
            if mask & (1 << index) != 0 {
                progress = progress.observe(*milestone);
            }
        }
        for completion in [
            CallCompletion::Cancelled,
            CallCompletion::Ended,
            CallCompletion::StasisExited,
            CallCompletion::Failed,
        ] {
            let terminal = progress.finish(completion);
            assert_eq!(terminal.completion(), Some(completion));
            for milestone in MILESTONES {
                assert_eq!(terminal.observe(milestone), terminal);
            }
            for other in [
                CallCompletion::Cancelled,
                CallCompletion::Ended,
                CallCompletion::StasisExited,
                CallCompletion::Failed,
            ] {
                assert_eq!(terminal.finish(other), terminal);
            }
        }
    }
}
