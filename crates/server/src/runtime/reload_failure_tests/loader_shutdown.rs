use super::{combine, configuration, failure, joined, Server};
use crate::{reload::PublicationPort, ControlError};
use rusterisk_config::ConfigError;
use std::{path::Path, sync::mpsc, time::Duration};
use tokio::{sync::oneshot, time::Instant};

const PRIMARY: &str = "HTTP settings coordinator stopped";

fn require(condition: bool, message: &'static str) -> Result<(), ControlError> {
    condition
        .then_some(())
        .ok_or(ControlError::Invalid(message))
}

fn checkpoint_failure(error: impl std::fmt::Display) -> ConfigError {
    ConfigError {
        file: "<shutdown loader fixture>".into(),
        line: 0,
        reason: format!("Owned loader checkpoint failed: {error}"),
    }
}

async fn bounded<T>(
    future: impl std::future::Future<Output = Result<T, ControlError>>,
) -> Result<T, ControlError> {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .map_err(failure)?
}

async fn exercise(data: &Path) -> Result<(), ControlError> {
    let replacement = configuration("replacement-password")?;
    let mut server = Server::bind(configuration("starting-password")?, 4, data.to_owned()).await?;
    let controller = server.controller();
    let addresses = (|| Ok::<_, ControlError>((server.local_addr()?, server.sip_listeners()?)))();
    let generation = server.signaling.configuration_generation();
    let (port, receiver) = PublicationPort::channel();
    server.http = server.http.with_publication(receiver, generation);
    drop(port);
    let (deadline, observed_deadline) = oneshot::channel();
    server.shutdown_checkpoint = Some(deadline);
    let (reply, caller) = oneshot::channel();
    let mut caller = Some(caller);
    let (started, observed_start) = oneshot::channel();
    let (release, waiting) = mpsc::sync_channel(1);
    server
        .signaling
        .control_for_test()
        .begin_loading_for_shutdown_test(
            data.to_owned(),
            reply,
            ControlError::Adapter(PRIMARY.into()),
            move |_directory| {
                started.send(()).map_err(|()| {
                    checkpoint_failure("fixture stopped before observing the started loader")
                })?;
                waiting
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(checkpoint_failure)?;
                Ok(replacement)
            },
        );
    // The loader starts before the server, so that the shutdown cannot cancel it before it runs.
    let started = bounded(async { observed_start.await.map_err(failure) }).await;
    let task = tokio::spawn(server.run());
    let primary = async {
        addresses.as_ref().map_err(Clone::clone)?;
        started?;
        let deadline = bounded(async { observed_deadline.await.map_err(failure) }).await?;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(11)).await;
        let expired = Instant::now() >= deadline;
        tokio::time::resume();
        require(
            expired,
            "Shutdown deadline was not crossed by the fixture clock",
        )?;
        let observed = tokio::time::timeout(
            Duration::from_millis(100),
            caller.as_mut().ok_or(ControlError::Capacity)?,
        )
        .await;
        if let Ok(result) = observed {
            drop(caller.take());
            return Err(failure(format!(
                "Control lost its caller before the started loader was reaped: {result:?}"
            )));
        }
        require(
            !task.is_finished(),
            "Server exited before the started loader was reaped",
        )
    }
    .await;
    controller.shutdown();
    let primary = combine(primary, release.send(()).map_err(failure));
    let server_result = joined(task).await;
    let preserved = match server_result {
        Ok(result) => require(
            result == Err(ControlError::Adapter(PRIMARY.into())),
            "Shutdown replaced the first component failure",
        ),
        Err(error) => Err(error),
    };
    let mut result = combine(primary, preserved);
    if let Some(caller) = caller {
        let preserved = bounded(async { caller.await.map_err(failure) })
            .await
            .and_then(|result| {
                require(
                    result == Err(ControlError::Adapter(PRIMARY.into())),
                    "Control shutdown replaced the loader caller's primary failure",
                )
            });
        result = combine(result, preserved);
    }
    match addresses {
        Ok((http, listeners)) => {
            result = combine(
                result,
                std::net::TcpListener::bind(http).map(drop).map_err(failure),
            );
            for (_, address) in listeners {
                result = combine(
                    result,
                    tokio::net::UdpSocket::bind(address)
                        .await
                        .map(drop)
                        .map_err(failure),
                );
            }
        }
        Err(error) => result = combine(result, Err(error)),
    }
    combine(
        result,
        rusterisk_storage::Store::open(data)
            .map(drop)
            .map_err(failure),
    )
}

#[tokio::test]
async fn shutdown_deadlines_preserve_the_control_owner_until_its_loader_is_reaped(
) -> Result<(), ControlError> {
    let data = tempfile::tempdir().map_err(failure)?;
    let result = exercise(data.path()).await;
    combine(result, data.close().map_err(failure))
}
