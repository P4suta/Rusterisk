use super::failure;
use rusterisk_server::ControlError;
use rusterisk_test_support::Wait;
use std::fmt::Display;
use tokio::task::JoinHandle;

pub(super) async fn finish<T, E: Display>(
    mut task: JoinHandle<Result<T, E>>,
) -> Result<T, ControlError> {
    match Wait::Shutdown.bounded(&mut task).await {
        Ok(result) => result.map_err(failure)?.map_err(failure),
        Err(primary) => {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Err(failure(primary)),
                Err(error) => Err(failure(format!("{primary}; cleanup failed: {error}"))),
                Ok(Err(error)) => Err(failure(format!("{primary}; worker failed: {error}"))),
                Ok(Ok(_)) => Err(failure(primary)),
            }
        }
    }
}

pub(super) fn combine<T>(
    primary: Result<T, ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<T, ControlError> {
    match (primary, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => {
            Err(failure(format!("{primary}; cleanup failed: {cleanup}")))
        }
    }
}
