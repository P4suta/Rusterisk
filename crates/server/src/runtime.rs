use crate::encryption::{TlsClientContext, TlsContext};
use crate::listener::HttpServer;
use crate::media::MediaHub;
use crate::signaling::Signaling;
use crate::storage::StorageWorker;
use crate::{ControlError, Controller, Runner};
use rusterisk_config::Config;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::task::JoinSet;

#[cfg(test)]
mod idle_tests;
#[cfg(test)]
#[path = "runtime/reload_failure_tests.rs"]
mod reload_failure_tests;

#[derive(Clone, Copy, Debug)]
enum Component {
    Control,
    Signaling,
    Media,
    Http,
}

pub struct Server {
    controller: Controller,
    control: Runner,
    signaling: Signaling,
    media: MediaHub,
    http: HttpServer,
    storage: StorageWorker,
    #[cfg(test)]
    shutdown_checkpoint: Option<tokio::sync::oneshot::Sender<tokio::time::Instant>>,
}

struct StartupConfiguration {
    config: Config,
    directory: Option<PathBuf>,
}

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

async fn close_startup(storage: StorageWorker, primary: ControlError) -> ControlError {
    match storage.close().await {
        Ok(()) => primary,
        Err(cleanup) => failure(format!(
            "{primary}; storage startup cleanup failed: {cleanup}"
        )),
    }
}

impl Server {
    pub async fn validate(config: &Config) -> Result<(), ControlError> {
        Self::validate_with_data_directory(config, Path::new("data")).await
    }

    pub async fn validate_with_data_directory(
        config: &Config,
        data_directory: &Path,
    ) -> Result<(), ControlError> {
        if let Some(files) = &config.http.tls {
            TlsContext::load(files).await?;
        }
        for transport in &config.transports {
            if let Some(files) = &transport.tls {
                TlsContext::load(files).await?;
            }
        }
        let mut native_roots_loaded = false;
        let signaling_roots = config
            .transports
            .iter()
            .filter(|transport| transport.protocol == rusterisk_config::Protocol::Tls)
            .map(|transport| transport.ca_list_file.as_deref());
        let media_roots = config
            .websocket
            .clients
            .iter()
            .filter(|client| client.requires_tls())
            .map(|client| client.ca_list_file.as_deref());
        for roots in signaling_roots.chain(media_roots) {
            if roots.is_some() || !native_roots_loaded {
                TlsClientContext::load(roots)?;
                native_roots_loaded |= roots.is_none();
            }
        }
        crate::storage::check_music(data_directory, config.music_on_hold.clone())
            .map_err(failure)?;
        Ok(())
    }

    pub fn bind(
        config: Config,
        capacity: usize,
        data_directory: PathBuf,
    ) -> impl std::future::Future<Output = Result<Self, ControlError>> {
        Self::bind_with_source(
            Ok(StartupConfiguration {
                config,
                directory: None,
            }),
            capacity,
            data_directory,
        )
    }

    pub fn bind_from_directory(
        directory: PathBuf,
        capacity: usize,
        data_directory: PathBuf,
    ) -> impl std::future::Future<Output = Result<Self, ControlError>> {
        let startup = (|| {
            let config = crate::runner::load_startup_configuration(&directory)
                .map_err(ControlError::Configuration)?;
            let directory = std::fs::canonicalize(directory).map_err(failure)?;
            config
                .validate_runtime()
                .map_err(ControlError::Configuration)?;
            Ok(StartupConfiguration {
                config,
                directory: Some(directory),
            })
        })();
        Self::bind_with_source(startup, capacity, data_directory)
    }

    async fn bind_with_source(
        startup: Result<StartupConfiguration, ControlError>,
        capacity: usize,
        data_directory: PathBuf,
    ) -> Result<Self, ControlError> {
        let StartupConfiguration {
            mut config,
            directory,
        } = startup?;
        let (storage_port, storage) = StorageWorker::start(data_directory)
            .await
            .map_err(failure)?;
        let (controller, control, effects) = Controller::new(config.clone(), capacity);
        let (media_port, media, notices) = MediaHub::new(capacity);
        let music = match storage_port
            .prepare_music(config.music_on_hold.clone())
            .await
        {
            Ok(music) => music,
            Err(primary) => return Err(close_startup(storage, failure(primary)).await),
        };
        let control = match control
            .with_music(music)
            .and_then(|control| control.with_storage(storage_port.clone()))
        {
            Ok(control) => control,
            Err(primary) => return Err(close_startup(storage, primary).await),
        };
        let http = match HttpServer::bind(&config.http, controller.clone()).await {
            Ok(http) => http.with_media(media_port.clone()),
            Err(primary) => return Err(close_startup(storage, primary).await),
        };
        config.http.bind = match http.local_addr() {
            Ok(address) => address,
            Err(primary) => {
                drop(http);
                return Err(close_startup(storage, primary).await);
            }
        };
        let signaling = match Signaling::bind(
            config.clone(),
            controller.clone(),
            effects,
            media_port.clone(),
            notices,
        )
        .await
        {
            Ok(signaling) => signaling.with_storage(storage_port),
            Err(primary) => {
                drop(http);
                return Err(close_startup(storage, primary).await);
            }
        };
        let control = match control.with_native_profiles(signaling.native_profiles()) {
            Ok(control) => control,
            Err(primary) => {
                drop(signaling);
                drop(http);
                return Err(close_startup(storage, primary).await);
            }
        };
        let control = match control.with_websocket_profiles(signaling.websocket_profiles()) {
            Ok(control) => control,
            Err(primary) => {
                drop(signaling);
                drop(http);
                return Err(close_startup(storage, primary).await);
            }
        };
        let generation = control.configuration_generation();
        let (http_publication, http_receiver) = crate::reload::PublicationPort::channel();
        let (native_publication, native_receiver) = crate::reload::PublicationPort::channel();
        let control = match directory {
            Some(directory) => control.with_reload_source(
                signaling.native_preparation(),
                http_publication,
                native_publication,
                Some(directory),
            ),
            None => control.with_reload(
                signaling.native_preparation(),
                http_publication,
                native_publication,
            ),
        };
        let signaling = signaling.with_publication(native_receiver, generation);
        let http = http.with_signaling(signaling.websocket_port());
        let http = http.with_publication(http_receiver, generation);
        Ok(Self {
            controller,
            control,
            signaling,
            media,
            http,
            storage,
            #[cfg(test)]
            shutdown_checkpoint: None,
        })
    }

    pub fn controller(&self) -> Controller {
        self.controller.clone()
    }
    pub fn local_addr(&self) -> Result<SocketAddr, ControlError> {
        self.http.local_addr()
    }
    pub fn sip_listeners(&self) -> Result<Vec<(String, SocketAddr)>, ControlError> {
        self.signaling.listeners()
    }

    pub async fn run(self) -> Result<(), ControlError> {
        let Self {
            controller,
            control,
            signaling,
            media,
            http,
            storage,
            #[cfg(test)]
            shutdown_checkpoint,
        } = self;
        let mut tasks = JoinSet::new();
        tasks.spawn(async move { (Component::Control, control.run().await) });
        let stopped = controller.shutdown_signal();
        let signaling_task =
            tasks.spawn(async move { (Component::Signaling, signaling.run(stopped).await) });
        let stopped = controller.shutdown_signal();
        let media_task = tasks
            .spawn(async move { (Component::Media, media.run(stopped).await.map_err(failure)) });
        let http_task = tasks.spawn(async move { (Component::Http, http.run().await) });
        let cancellable = [signaling_task, media_task, http_task];
        let mut primary = match tasks.join_next().await {
            Some(Ok((component, Ok(())))) => {
                if controller.is_stopping() {
                    None
                } else {
                    Some(ControlError::Adapter(format!(
                        "{component:?} stopped unexpectedly"
                    )))
                }
            }
            Some(Ok((component, Err(error)))) => {
                tracing::error!(?component, %error, "server component failed");
                Some(error)
            }
            Some(Err(error)) => Some(failure(error)),
            None => Some(ControlError::Adapter("No server task running".into())),
        };
        controller.shutdown();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        #[cfg(test)]
        if let Some(checkpoint) = shutdown_checkpoint {
            if checkpoint.send(deadline).is_err() {
                tracing::debug!("Shutdown deadline observer disconnected");
            }
        }
        while !tasks.is_empty() {
            match tokio::time::timeout_at(deadline, tasks.join_next()).await {
                Ok(Some(Ok((_, Ok(()))))) | Ok(None) => {}
                Ok(Some(Ok((component, Err(error))))) => {
                    tracing::error!(?component, %error, "server component cleanup failed");
                    if primary.is_none() {
                        primary = Some(error);
                    }
                }
                Ok(Some(Err(error))) => {
                    tracing::error!(%error, "server task cleanup failed");
                    if primary.is_none() {
                        primary = Some(failure(error));
                    }
                }
                Err(error) => {
                    for component in &cancellable {
                        component.abort();
                    }
                    while let Some(joined) = tasks.join_next().await {
                        match joined {
                            Ok((_, Ok(()))) => {}
                            Ok((component, Err(error))) => {
                                tracing::error!(?component, %error, "server component failed before cancellation")
                            }
                            Err(error) => {
                                tracing::debug!(%error, "server task cancelled during shutdown")
                            }
                        }
                    }
                    if primary.is_none() {
                        primary = Some(failure(error));
                    }
                }
            }
        }
        if let Err(error) = storage.close().await {
            tracing::error!(%error, "storage shutdown failed");
            if primary.is_none() {
                primary = Some(failure(error));
            }
        }
        match primary {
            None => Ok(()),
            Some(error) => Err(error),
        }
    }
}
