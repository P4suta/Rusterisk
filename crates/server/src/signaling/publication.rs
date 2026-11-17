use super::{registrations, transport, websocket, Signaling};
use crate::reload::{
    PreparedHandoff, PublicationCommand, PublicationPhase, PublicationReceiver, PublicationState,
    PUBLICATION_BUDGET,
};
use crate::{
    call_profiles::{BoundNativeProfiles, NativeProfileCatalog},
    ControlError,
};
use rusterisk_config::Config;
use rusterisk_core::SettingsGeneration;
use rusterisk_sip::{AdmissionMode, PreparedEngineConfiguration};
use std::net::SocketAddr;
use tokio::sync::{mpsc, watch};

#[cfg(test)]
mod shutdown_tests;

#[derive(Clone)]
pub(crate) struct NativePreparation {
    bindings: BoundNativeProfiles,
    http: SocketAddr,
}

pub(crate) struct NativePreparedSignaling {
    config: Config,
    bindings: BoundNativeProfiles,
    catalog: NativeProfileCatalog,
    clients: websocket::PreparedClients,
    engines: Vec<PreparedEngineConfiguration>,
    network: transport::PreparedNetworkSettings,
    registrations: registrations::PreparedRegistrations,
}

pub(super) struct NativePublication {
    pub(super) receiver: PublicationReceiver<NativePreparedSignaling>,
    state: PublicationState<NativePreparedSignaling>,
}

pub(super) async fn receive<T>(
    receiver: Option<&mut mpsc::Receiver<T>>,
    stopped: &watch::Receiver<bool>,
) -> Result<Option<T>, ControlError> {
    match receiver {
        Some(receiver) => match receiver.recv().await {
            Some(value) => Ok(Some(value)),
            None if *stopped.borrow() => Ok(None),
            None => Err(ControlError::Adapter(
                "Native settings coordinator stopped".into(),
            )),
        },
        None => std::future::pending().await,
    }
}

impl NativePreparation {
    pub(crate) async fn prepare(
        &self,
        config: &Config,
    ) -> Result<NativePreparedSignaling, ControlError> {
        config.validate_runtime().map_err(super::failure)?;
        let mut projected = self.bindings.project(config)?;
        projected.http.bind = self.http;
        let (network, contexts) =
            transport::PreparedNetworkSettings::prepare(&projected.transports).await?;
        let bindings = self
            .bindings
            .replacing_contexts(&config.transports, contexts)?;
        let mut catalog = NativeProfileCatalog::new(config);
        catalog.bind(bindings.clone())?;
        let clients = websocket::PreparedClients::new(&config.websocket.clients)?;
        let registrations = registrations::PreparedRegistrations::prepare(&projected)?;
        let engines = projected
            .transports
            .iter()
            .map(|transport| {
                let mut scoped = projected.clone();
                scoped
                    .endpoints
                    .retain(|endpoint| endpoint.transport == transport.name);
                PreparedEngineConfiguration::prepare(scoped).map_err(super::failure)
            })
            .collect::<Result<Vec<_>, ControlError>>()?;
        Ok(NativePreparedSignaling {
            config: projected,
            bindings,
            catalog,
            clients,
            engines,
            network,
            registrations,
        })
    }
}

impl NativePreparedSignaling {
    pub(crate) fn call_catalog(&self) -> NativeProfileCatalog {
        self.catalog.clone()
    }

    pub(crate) fn websocket_catalog(&self) -> websocket::PreparedClients {
        self.clients.clone()
    }
}

impl Signaling {
    pub(crate) fn with_publication(
        mut self,
        receiver: PublicationReceiver<NativePreparedSignaling>,
        generation: SettingsGeneration,
    ) -> Self {
        self.registrations.settings_epoch = Some(generation);
        self.publication = Some(NativePublication {
            receiver,
            state: PublicationState::new(generation),
        });
        self
    }

    pub(super) fn admitting(&self) -> bool {
        self.publication
            .as_ref()
            .is_none_or(|publication| publication.state.admitting())
    }

    pub(super) fn stage_native_settings(
        &mut self,
        prepared: PreparedHandoff<NativePreparedSignaling>,
    ) -> Result<(), ControlError> {
        if prepared.reply.is_closed() {
            return Ok(());
        }
        let result = (|| {
            let publication = self.publication.as_mut().ok_or(ControlError::Unsupported)?;
            if !self
                .startup_native_profiles
                .same_listeners(&prepared.payload.bindings)
                || prepared.payload.config.http.bind != self.config.http.bind
                || prepared.payload.engines.len() != self.listeners.len()
            {
                return Err(ControlError::Conflict(
                    "Signaling listener replacement requires restart",
                ));
            }
            prepared.payload.network.validate(&self.network)?;
            publication.state.stage(prepared.key, prepared.payload)
        })();
        prepared.reply.complete(result);
        Ok(())
    }

    pub(super) fn native_publication(
        &mut self,
        command: PublicationCommand,
        now: u64,
    ) -> Result<(), ControlError> {
        let publication = self.publication.as_mut().ok_or(ControlError::Unsupported)?;
        let result = (|| match command.phase {
            PublicationPhase::Staged => {
                Err(ControlError::Invalid("Native settings payload is required"))
            }
            PublicationPhase::Quiesce => {
                if !self.registrations.quiesced(command.key) {
                    let prepared = publication.state.prepared(command.key)?;
                    let budget =
                        u64::try_from(PUBLICATION_BUDGET.as_millis()).map_err(super::failure)?;
                    let clock_limit = now.checked_add(budget).ok_or(ControlError::Capacity)?;
                    self.registrations.quiesce(
                        command.key,
                        &prepared.registrations,
                        &self.network,
                        now,
                        clock_limit,
                    )?;
                }
                publication.state.quiesce(command.key)?;
                for listener in &mut self.listeners {
                    listener.engine.set_admission(AdmissionMode::Quiesced);
                }
                Ok(())
            }
            PublicationPhase::Publish(generation) => {
                let prepared = publication.state.publish(command.key, generation)?;
                self.registrations
                    .publish(command.key, prepared.registrations, generation, now)?;
                self.network.publish_settings(prepared.network)?;
                if prepared.engines.len() != self.listeners.len() {
                    return Err(ControlError::Conflict(
                        "Signaling engines changed during publication",
                    ));
                }
                for (listener, config) in self.listeners.iter_mut().zip(prepared.engines) {
                    listener.engine.publish_configuration(config);
                }
                self.config = prepared.config;
                self.native_profiles = prepared.bindings;
                self.websocket_clients = prepared.clients;
                Ok(())
            }
            PublicationPhase::Resume(generation) => {
                publication.state.resume(command.key, generation)?;
                for listener in &mut self.listeners {
                    listener.engine.set_admission(AdmissionMode::Open);
                }
                Ok(())
            }
            PublicationPhase::Abort => {
                publication.state.abort(command.key)?;
                self.registrations.abort_publication(command.key)?;
                for listener in &mut self.listeners {
                    listener.engine.set_admission(AdmissionMode::Open);
                }
                Ok(())
            }
        })();
        let fatal = match command.phase {
            PublicationPhase::Publish(_) | PublicationPhase::Resume(_) => {
                result.as_ref().err().cloned()
            }
            PublicationPhase::Staged | PublicationPhase::Quiesce | PublicationPhase::Abort => None,
        };
        command.reply.complete(result);
        match fatal {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub(crate) fn native_preparation(&self) -> NativePreparation {
        NativePreparation {
            bindings: self.startup_native_profiles.clone(),
            http: self.config.http.bind,
        }
    }

    pub(crate) fn websocket_profiles(&self) -> websocket::PreparedClients {
        self.websocket_clients.clone()
    }
}
