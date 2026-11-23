use rusterisk_core::Wake;

pub(super) const RETRY_MS: u64 = 100;

// State that a settle acts on at any instant, such as a pending reply whose channel ended.
pub(super) const DUE: Wake = Wake::At(0);

#[derive(Default)]
pub(super) struct Overdue {
    deferred: Option<(Wake, u64)>,
}

impl Overdue {
    pub(super) fn timer(&self, wake: Wake) -> Wake {
        match self.deferred {
            Some((deferred, retry)) if deferred == wake => Wake::At(retry),
            _ => wake,
        }
    }

    pub(super) fn settled(&mut self, before: Wake, after: Wake, now: u64) {
        let unconsumed = after == before && matches!(after, Wake::At(at) if at <= now);
        if unconsumed && self.deferred.map(|(deferred, _)| deferred) != Some(after) {
            tracing::debug!(retry_ms = RETRY_MS, "a due control wake stayed unconsumed");
        }
        self.deferred = unconsumed.then(|| (after, now.saturating_add(RETRY_MS)));
    }
}

#[cfg(test)]
mod tests {
    use super::{Overdue, DUE, RETRY_MS};
    use rusterisk_core::Wake;

    #[test]
    fn a_wake_that_a_settle_consumed_keeps_its_own_instant() {
        let mut overdue = Overdue::default();
        overdue.settled(Wake::At(40), Wake::At(90), 40);
        assert_eq!(overdue.timer(Wake::At(90)), Wake::At(90));
        overdue.settled(Wake::At(90), Wake::Idle, 90);
        assert_eq!(overdue.timer(Wake::Idle), Wake::Idle);
        overdue.settled(Wake::Now, Wake::Now, 90);
        assert_eq!(overdue.timer(Wake::Now), Wake::Now);
    }

    #[test]
    fn an_unconsumed_due_wake_retries_after_one_interval_instead_of_stopping() {
        let mut overdue = Overdue::default();
        for (wake, now) in [(Wake::At(40), 40), (Wake::At(40), 75), (DUE, 75)] {
            overdue.settled(wake, wake, now);
            assert_eq!(overdue.timer(wake), Wake::At(now + RETRY_MS));
        }
    }

    #[test]
    fn a_different_wake_replaces_a_deferred_one() {
        let mut overdue = Overdue::default();
        overdue.settled(Wake::At(40), Wake::At(40), 40);
        assert_eq!(overdue.timer(Wake::At(60)), Wake::At(60));
        assert_eq!(overdue.timer(Wake::Now), Wake::Now);
        assert_eq!(overdue.timer(Wake::Idle), Wake::Idle);
    }

    #[test]
    fn a_wake_that_is_still_ahead_keeps_its_instant() {
        let mut overdue = Overdue::default();
        overdue.settled(Wake::At(500), Wake::At(500), 40);
        assert_eq!(overdue.timer(Wake::At(500)), Wake::At(500));
    }

    #[test]
    fn a_due_wake_that_the_settle_changed_runs_without_a_delay() {
        let mut overdue = Overdue::default();
        overdue.settled(Wake::At(10), Wake::At(20), 50);
        assert_eq!(overdue.timer(Wake::At(20)), Wake::At(20));
        overdue.settled(Wake::At(900), DUE, 50);
        assert_eq!(overdue.timer(DUE), DUE);
    }
}
