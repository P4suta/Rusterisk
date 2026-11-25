use super::receive;
use crate::ControlError;
use rusterisk_test_support::Wait;
use tokio::sync::{mpsc, watch};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

#[tokio::test]
async fn coordinator_close_reads_the_current_stop_value_after_waiting() -> Result<(), ControlError>
{
    let (sender, mut receiver) = mpsc::channel::<u8>(1);
    let (stop, stopped) = watch::channel(false);
    let received = receive(Some(&mut receiver), &stopped);
    tokio::pin!(received);
    assert!(futures_util::poll!(received.as_mut()).is_pending());
    stop.send_replace(true);
    drop(sender);
    let received: Option<u8> = Wait::Ready.bounded(received).await.map_err(failure)??;
    assert!(received.is_none());
    Ok(())
}

#[tokio::test]
async fn coordinator_close_while_running_remains_an_error() -> Result<(), ControlError> {
    let (sender, mut receiver) = mpsc::channel::<u8>(1);
    let (_stop, stopped) = watch::channel(false);
    let received = receive(Some(&mut receiver), &stopped);
    tokio::pin!(received);
    assert!(futures_util::poll!(received.as_mut()).is_pending());
    drop(sender);
    let received = Wait::Ready.bounded(received).await.map_err(failure)?;
    assert!(
        matches!(received, Err(ControlError::Adapter(message)) if message == "Native settings coordinator stopped")
    );
    Ok(())
}

#[tokio::test]
async fn an_unconfigured_publication_receiver_stays_pending_during_shutdown() {
    let (_stop, stopped) = watch::channel(true);
    let received = receive::<u8>(None, &stopped);
    tokio::pin!(received);
    assert!(futures_util::poll!(received.as_mut()).is_pending());
}

#[tokio::test]
async fn received_values_are_preserved_while_stopping() -> Result<(), ControlError> {
    let (sender, mut receiver) = mpsc::channel(1);
    let (stop, stopped) = watch::channel(false);
    let received = receive(Some(&mut receiver), &stopped);
    tokio::pin!(received);
    assert!(futures_util::poll!(received.as_mut()).is_pending());
    stop.send_replace(true);
    sender.try_send(17_u8).map_err(failure)?;
    let received: Option<u8> = Wait::Ready.bounded(received).await.map_err(failure)??;
    assert_eq!(received, Some(17));
    Ok(())
}
