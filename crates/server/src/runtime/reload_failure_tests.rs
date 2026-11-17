use super::Server;
use crate::reload::{PublicationPhase, PublicationPort, PublicationReceiver, PUBLICATION_BUDGET};
use crate::signaling::NativePreparedSignaling;
use crate::{ControlError, Controller};
use rusterisk_config::Config;
use std::{path::Path, time::Duration};
use tokio::task::JoinHandle;

const PRIMARY: &str = "Native publication acknowledgement failed after the first swap";

#[path = "reload_failure_tests/loader_shutdown.rs"]
mod loader_shutdown;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn combine<T>(
    primary: Result<T, ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<T, ControlError> {
    match (primary, cleanup) {
        (primary, Ok(())) => primary,
        (Ok(_), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(failure(format!(
            "{primary}; reload fixture cleanup failed: {cleanup}"
        ))),
    }
}

fn configuration(password: &str) -> Result<Config, ControlError> {
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        &format!("[app]\ntype=user\npassword={password}\n"),
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n",
        "",
        "",
    )
    .map_err(failure)?;
    config.http.bind.set_port(0);
    Ok(config)
}

async fn acknowledged(
    result: Result<crate::reload::Acknowledgement, ControlError>,
) -> Result<(), ControlError> {
    tokio::time::timeout(PUBLICATION_BUDGET, result?.receive())
        .await
        .map_err(failure)?
}

async fn participant(
    port: PublicationPort<NativePreparedSignaling>,
    mut receiver: PublicationReceiver<NativePreparedSignaling>,
    controller: Controller,
) -> Result<(), ControlError> {
    let mut stopped = controller.shutdown_signal();
    loop {
        if *stopped.borrow() {
            return Ok(());
        }
        tokio::select! {
            _changed = stopped.changed() => return Ok(()),
            prepared = receiver.prepared.recv() => {
                let prepared = match prepared {
                    Some(prepared) => prepared,
                    None if *stopped.borrow() => return Ok(()),
                    None => return Err(ControlError::Conflict("Prepared handoff closed")),
                };
                let result = acknowledged(port.stage(prepared.key, prepared.payload)).await;
                prepared.reply.complete(result);
            }
            command = receiver.commands.recv() => {
                let command = match command {
                    Some(command) => command,
                    None if *stopped.borrow() => return Ok(()),
                    None => return Err(ControlError::Conflict("Publication commands closed")),
                };
                let mut result = acknowledged(port.command(command.key, command.phase)).await;
                if result.is_ok() && matches!(command.phase, PublicationPhase::Publish(_)) {
                    result = Err(failure(PRIMARY));
                }
                command.reply.complete(result);
            }
        }
    }
}

async fn joined(
    mut task: JoinHandle<Result<(), ControlError>>,
) -> Result<Result<(), ControlError>, ControlError> {
    match tokio::time::timeout(Duration::from_secs(10), &mut task).await {
        Ok(result) => result.map_err(failure),
        Err(primary) => {
            task.abort();
            let cleanup = match task.await {
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(failure(error)),
                Ok(result) => result,
            };
            combine(Err(failure(primary)), cleanup)
        }
    }
}

fn contains_primary(result: &Result<(), ControlError>) -> bool {
    result
        .as_ref()
        .is_err_and(|error| error.to_string().contains(PRIMARY))
}

async fn exercise(data: &Path) -> Result<(), ControlError> {
    let replacement = configuration("replacement-password")?;
    let mut server = Server::bind(configuration("starting-password")?, 4, data.to_owned()).await?;
    let controller = server.controller();
    let generation = server.control.configuration_generation();
    let preparation = server.signaling.native_preparation();
    let (http_port, http_receiver) = PublicationPort::channel();
    let (native_port, native_receiver) = PublicationPort::channel();
    let (proxy_port, proxy_receiver) = PublicationPort::channel();
    server.http = server.http.with_publication(http_receiver, generation);
    server.signaling = server
        .signaling
        .with_publication(native_receiver, generation);
    server.control = server
        .control
        .with_reload(preparation, http_port.clone(), proxy_port);
    let proxy = tokio::spawn(participant(
        native_port.clone(),
        proxy_receiver,
        controller.clone(),
    ));
    let server_task = tokio::spawn(server.run());
    let reload = controller.reload(replacement).await;
    controller.shutdown();
    let server_result = joined(server_task).await;
    let proxy_result = joined(proxy).await.and_then(|result| result);
    drop((http_port, native_port));
    let primary = if contains_primary(&reload) && server_result.as_ref().is_ok_and(contains_primary)
    {
        Ok(())
    } else {
        Err(failure(format!("The reload caller and server lost the publication failure: reload={reload:?}; server={server_result:?}")))
    };
    combine(primary, proxy_result)
}

#[tokio::test]
async fn publication_failure_reaches_the_reload_caller_and_server() -> Result<(), ControlError> {
    let data = tempfile::tempdir().map_err(failure)?;
    let result = exercise(data.path()).await;
    combine(result, data.close().map_err(failure))
}
