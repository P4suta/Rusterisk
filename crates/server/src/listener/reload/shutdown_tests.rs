use super::{publication_event, HttpPublication, PreparedHttpSettings};
use crate::{
    reload::{PublicationPort, PublicationState},
    ControlError,
};
use rusterisk_core::SettingsLifetimes;
use std::time::Duration;
use tokio::sync::watch;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn publication() -> (
    PublicationPort<PreparedHttpSettings>,
    Option<HttpPublication>,
) {
    let (port, receiver) = PublicationPort::channel();
    let settings = SettingsLifetimes::new(92);
    (
        port,
        Some(HttpPublication {
            receiver,
            state: PublicationState::new(settings.current()),
        }),
    )
}

#[tokio::test]
async fn coordinator_close_reads_the_current_stop_value_after_waiting() -> Result<(), ControlError>
{
    let (port, mut publication) = publication();
    let (stop, stopped) = watch::channel(false);
    let received = publication_event(&mut publication, &stopped);
    tokio::pin!(received);
    assert!(futures_util::poll!(received.as_mut()).is_pending());
    stop.send_replace(true);
    drop(port);
    let received: Option<_> = tokio::time::timeout(Duration::from_secs(1), received)
        .await
        .map_err(failure)??;
    assert!(received.is_none());
    Ok(())
}

#[tokio::test]
async fn coordinator_close_while_running_remains_an_error() -> Result<(), ControlError> {
    let (port, mut publication) = publication();
    let (_stop, stopped) = watch::channel(false);
    let received = publication_event(&mut publication, &stopped);
    tokio::pin!(received);
    assert!(futures_util::poll!(received.as_mut()).is_pending());
    drop(port);
    let received = tokio::time::timeout(Duration::from_secs(1), received)
        .await
        .map_err(failure)?;
    assert!(
        matches!(received, Err(ControlError::Adapter(message)) if message == "HTTP settings coordinator stopped")
    );
    Ok(())
}

#[tokio::test]
async fn an_unconfigured_publication_receiver_stays_pending_during_shutdown() {
    let (_stop, stopped) = watch::channel(true);
    let mut publication = None;
    let received = publication_event(&mut publication, &stopped);
    tokio::pin!(received);
    assert!(futures_util::poll!(received.as_mut()).is_pending());
}
