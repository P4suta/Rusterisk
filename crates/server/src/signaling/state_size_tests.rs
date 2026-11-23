use super::Signaling;
use crate::ControlError;
use std::future::Future;
use tokio::sync::watch;

const STATE_BUDGET: usize = 16 * 1024;
const TASK_BUDGET: usize = 48 * 1024;

fn future_size<F>(_: fn(Signaling, watch::Receiver<bool>) -> F) -> usize
where
    F: Future<Output = Result<(), ControlError>>,
{
    std::mem::size_of::<F>()
}

#[test]
fn the_signaling_state_stays_within_its_size_budget() {
    let size = std::mem::size_of::<Signaling>();
    assert!(size <= STATE_BUDGET, "the signaling state is {size} bytes");
}

#[test]
fn the_signaling_task_stays_within_its_size_budget() {
    let size = future_size(Signaling::run);
    assert!(
        size <= TASK_BUDGET,
        "the signaling task future is {size} bytes"
    );
}
