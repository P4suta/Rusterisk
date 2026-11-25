use super::sounds;
use rusterisk_client::{Client, Error};
use rusterisk_config::Config;
use rusterisk_server::test_time::Wait;
use rusterisk_server::{runtime::Server, ControlError, Controller};
use std::{net::SocketAddr, path::Path};
use tokio::task::JoinHandle;

#[path = "runtime/contracts.rs"]
mod contracts;

pub(super) struct Fixture {
    client: Client,
    controller: Controller,
    address: SocketAddr,
    listeners: Vec<(String, SocketAddr)>,
    task: JoinHandle<Result<(), ControlError>>,
}

fn failure(error: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(error.to_string()))
}

fn combine(primary: Result<(), Error>, cleanup: Result<(), Error>) -> Result<(), Error> {
    match (primary, cleanup) {
        (primary, Ok(())) => primary,
        (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(Error::Cleanup(format!(
            "{primary}; reference runtime cleanup failed: {cleanup}"
        ))),
    }
}

async fn shutdown(
    controller: Controller,
    mut task: JoinHandle<Result<(), ControlError>>,
) -> Result<(), Error> {
    controller.shutdown();
    match Wait::Shutdown.bounded(&mut task).await {
        Ok(result) => result.map_err(failure)?.map_err(failure),
        Err(_) => {
            task.abort();
            let cleanup = match task.await {
                Ok(result) => result.map_err(failure),
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(failure(error)),
            };
            combine(Err(Error::Timeout), cleanup)
        }
    }
}

impl Fixture {
    pub(super) async fn start(config: Config, configuration: &Path) -> Result<Self, Error> {
        let source = std::fs::read_to_string(configuration.join("native/config/musiconhold.conf"))?;
        let config = config
            .with_music_on_hold_configuration(&source)
            .map_err(failure)?;
        let data = configuration.join("native/data");
        let (storage, worker) = sounds::prepare(configuration, &data).await?;
        let closed = worker.close().await.map_err(failure);
        drop(storage);
        closed?;
        let server = Server::bind(config, 8, data).await.map_err(failure)?;
        let controller = server.controller();
        let observed = (|| {
            let address = server.local_addr().map_err(failure)?;
            let listeners = server.sip_listeners().map_err(failure)?;
            let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
            Ok::<_, Error>((client, address, listeners))
        })();
        let task = tokio::spawn(server.run());
        match observed {
            Ok((client, address, listeners)) => Ok(Self {
                client,
                controller,
                address,
                listeners,
                task,
            }),
            Err(primary) => match shutdown(controller, task).await {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(Error::Cleanup(format!(
                    "{primary}; reference runtime startup cleanup failed: {cleanup}"
                ))),
            },
        }
    }

    pub(super) const fn client(&self) -> &Client {
        &self.client
    }

    pub(super) const fn address(&self) -> SocketAddr {
        self.address
    }

    pub(super) const fn sip_listeners(&self) -> &[(String, SocketAddr)] {
        self.listeners.as_slice()
    }

    pub(super) async fn finish(self, primary: Result<(), Error>) -> Result<(), Error> {
        drop(self.client);
        combine(primary, shutdown(self.controller, self.task).await)
    }
}
