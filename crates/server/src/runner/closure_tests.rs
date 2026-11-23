use super::latch;
use std::future::Future;
use std::time::Duration;
use tokio::sync::oneshot;

const BUDGET: Duration = Duration::from_secs(5);

#[test]
fn every_closure_before_a_read_shares_one_pending_wake() {
    let (closures, mut closed) = latch();
    let notices = (0..=256).map(|_| closures.notice()).collect::<Vec<_>>();
    assert!(closed.try_recv().is_err());
    drop(notices);
    assert!(closed.try_recv().is_ok());
    assert!(closed.try_recv().is_err());
}

#[test]
fn a_closure_after_a_read_sets_the_next_wake() {
    let (closures, mut closed) = latch();
    drop(closures.notice());
    assert!(closed.try_recv().is_ok());
    drop(closures.notice());
    assert!(closed.try_recv().is_ok());
}

#[test]
fn a_closure_without_a_runner_ends_quietly() {
    let (closures, closed) = latch();
    let notice = closures.notice();
    drop(closed);
    drop(notice);
}

#[tokio::test(start_paused = true)]
async fn an_answered_wait_leaves_no_wake() {
    let (closures, mut closed) = latch();
    let (reply, receiver) = oneshot::channel();
    assert!(reply.send(7_u8).is_ok());
    assert!(matches!(
        closures.answered(BUDGET, receiver).await,
        Ok(Ok(7))
    ));
    assert!(closed.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn a_wait_whose_replier_left_leaves_no_wake() {
    let (closures, mut closed) = latch();
    let (reply, receiver) = oneshot::channel::<u8>();
    drop(reply);
    assert!(matches!(
        closures.answered(BUDGET, receiver).await,
        Ok(Err(_))
    ));
    assert!(closed.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn an_expired_wait_closes_its_reply_and_sets_one_wake() {
    let (closures, mut closed) = latch();
    let (reply, receiver) = oneshot::channel::<u8>();
    assert!(closures.answered(BUDGET, receiver).await.is_err());
    assert!(reply.is_closed());
    assert!(closed.try_recv().is_ok());
    assert!(closed.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn a_dropped_wait_closes_its_reply_and_sets_one_wake() {
    let (closures, mut closed) = latch();
    let (reply, receiver) = oneshot::channel::<u8>();
    {
        let mut wait = std::pin::pin!(closures.answered(BUDGET, receiver));
        let pending = std::future::poll_fn(|context| {
            std::task::Poll::Ready(wait.as_mut().poll(context).is_pending())
        })
        .await;
        assert!(pending);
        assert!(!reply.is_closed());
    }
    assert!(reply.is_closed());
    assert!(closed.try_recv().is_ok());
    assert!(closed.try_recv().is_err());
}
