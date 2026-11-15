use crate::encryption::TlsContext;
use crate::listener::HttpServer;
use crate::media::MediaHub;
use crate::signaling::Signaling;
use crate::storage::StorageWorker;
use crate::{ControlError, Controller, Runner};
use rusterisk_config::Config;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::task::JoinSet;

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
}

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

impl Server {
    pub async fn validate(config: &Config) -> Result<(), ControlError> {
        if let Some(files) = &config.http.tls {
            TlsContext::load(files).await?;
        }
        for transport in &config.transports {
            if let Some(files) = &transport.tls {
                TlsContext::load(files).await?;
            }
        }
        Ok(())
    }

    pub async fn bind(
        config: Config,
        capacity: usize,
        data_directory: PathBuf,
    ) -> Result<Self, ControlError> {
        let (storage_port, storage) = StorageWorker::start(data_directory)
            .await
            .map_err(failure)?;
        let (controller, control, effects) = Controller::new(config.clone(), capacity);
        let (media_port, media, notices) = MediaHub::new(capacity);
        let listeners: Result<(Runner, Signaling, HttpServer), ControlError> = async {
            let control = control.with_storage(storage_port.clone())?;
            let signaling = Signaling::bind(
                config.clone(),
                controller.clone(),
                effects,
                media_port,
                notices,
            )
            .await?
            .with_storage(storage_port);
            let http = HttpServer::bind(&config.http, controller.clone()).await?;
            Ok((control, signaling, http))
        }
        .await;
        let (control, signaling, http) = match listeners {
            Ok(listeners) => listeners,
            Err(primary) => {
                if let Err(error) = storage.close().await {
                    tracing::error!(%error, "storage startup cleanup failed");
                }
                return Err(primary);
            }
        };
        Ok(Self {
            controller,
            control,
            signaling,
            media,
            http,
            storage,
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
        } = self;
        let mut tasks = JoinSet::new();
        tasks.spawn(async move { (Component::Control, control.run().await) });
        let stopped = controller.shutdown_signal();
        tasks.spawn(async move { (Component::Signaling, signaling.run(stopped).await) });
        let stopped = controller.shutdown_signal();
        tasks.spawn(async move { (Component::Media, media.run(stopped).await.map_err(failure)) });
        tasks.spawn(async move { (Component::Http, http.run().await) });
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
                    tasks.abort_all();
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
