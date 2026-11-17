use super::{HttpServer, TlsContext};
use crate::reload::{
    PreparedHandoff, PublicationCommand, PublicationPhase, PublicationReceiver, PublicationState,
};
use crate::ControlError;
use rusterisk_config::Http;
use rusterisk_core::SettingsGeneration;
use std::net::SocketAddr;
use tokio::sync::watch;

#[cfg(test)]
mod shutdown_tests;

pub(crate) struct PreparedHttpSettings {
    bind: SocketAddr,
    encryption: Option<TlsContext>,
}

impl PreparedHttpSettings {
    pub(crate) async fn prepare(config: &Http) -> Result<Self, ControlError> {
        if !config.bind.ip().is_loopback() && config.tls.is_none() {
            return Err(ControlError::Forbidden);
        }
        let encryption = match &config.tls {
            Some(files) => Some(TlsContext::load(files).await?),
            None => None,
        };
        Ok(Self {
            bind: config.bind,
            encryption,
        })
    }
}

pub(super) struct HttpPublication {
    receiver: PublicationReceiver<PreparedHttpSettings>,
    state: PublicationState<PreparedHttpSettings>,
}

pub(super) enum PublicationEvent {
    Prepared(PreparedHandoff<PreparedHttpSettings>),
    Command(PublicationCommand),
}

impl HttpPublication {
    pub(super) fn admitting(&self) -> bool {
        self.state.admitting()
    }

    async fn next(
        &mut self,
        stopped: &watch::Receiver<bool>,
    ) -> Result<Option<PublicationEvent>, ControlError> {
        let event = tokio::select! {
            prepared = self.receiver.prepared.recv() => prepared.map(PublicationEvent::Prepared),
            command = self.receiver.commands.recv() => command.map(PublicationEvent::Command),
        };
        match event {
            Some(event) => Ok(Some(event)),
            None if *stopped.borrow() => Ok(None),
            None => Err(ControlError::Adapter(
                "HTTP settings coordinator stopped".into(),
            )),
        }
    }
}

pub(super) async fn publication_event(
    publication: &mut Option<HttpPublication>,
    stopped: &watch::Receiver<bool>,
) -> Result<Option<PublicationEvent>, ControlError> {
    match publication {
        Some(publication) => publication.next(stopped).await,
        None => std::future::pending().await,
    }
}

impl HttpServer {
    pub(crate) fn with_publication(
        mut self,
        receiver: PublicationReceiver<PreparedHttpSettings>,
        generation: SettingsGeneration,
    ) -> Self {
        self.publication = Some(HttpPublication {
            receiver,
            state: PublicationState::new(generation),
        });
        self
    }

    pub(super) fn publication(&mut self, event: PublicationEvent) -> Result<(), ControlError> {
        match event {
            PublicationEvent::Prepared(prepared) => self.stage_settings(prepared),
            PublicationEvent::Command(command) => {
                let publication = self.publication.as_mut().ok_or(ControlError::Unsupported)?;
                let result = match command.phase {
                    PublicationPhase::Staged => {
                        Err(ControlError::Invalid("HTTP settings payload is required"))
                    }
                    PublicationPhase::Quiesce => publication.state.quiesce(command.key),
                    PublicationPhase::Publish(generation) => publication
                        .state
                        .publish(command.key, generation)
                        .map(|prepared| self.encryption = prepared.encryption),
                    PublicationPhase::Resume(generation) => {
                        publication.state.resume(command.key, generation)
                    }
                    PublicationPhase::Abort => publication.state.abort(command.key),
                };
                command.reply.complete(result);
                Ok(())
            }
        }
    }

    fn stage_settings(
        &mut self,
        prepared: PreparedHandoff<PreparedHttpSettings>,
    ) -> Result<(), ControlError> {
        let publication = self.publication.as_mut().ok_or(ControlError::Unsupported)?;
        if prepared.reply.is_closed() {
            return Ok(());
        }
        let result = if prepared.payload.bind != self.requested_bind
            || prepared.payload.encryption.is_some() != self.encryption.is_some()
        {
            Err(ControlError::Conflict(
                "HTTP listener replacement requires restart",
            ))
        } else {
            publication.state.stage(prepared.key, prepared.payload)
        };
        prepared.reply.complete(result);
        Ok(())
    }
}
