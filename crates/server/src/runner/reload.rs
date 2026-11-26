use super::{Controller, Runner};
use crate::control::CallSettings;
use crate::listener::reload::PreparedHttpSettings;
use crate::reload::{
    Acknowledgement, PublicationKey, PublicationPhase, PublicationPort, PUBLICATION_BUDGET,
};
use crate::signaling::{NativePreparation, NativePreparedSignaling};
use crate::storage::StoragePort;
use crate::{Action, ControlError, Credentials};
use rusterisk_config::{Config, ConfigError, Reload};
use rusterisk_core::Wake;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinError, JoinSet};
use tokio::time::Instant;

const PREPARATION_BUDGET: Duration = Duration::from_secs(10);
const RELOAD_BUDGET: Duration = Duration::from_secs(25);

#[cfg(test)]
mod source_tests;

pub(super) fn new_settings(action: &Action) -> bool {
    match action {
        Action::Originate { .. }
        | Action::ExternalRtp { .. }
        | Action::ExternalAudioSocket { .. }
        | Action::ExternalWebSocket { .. }
        | Action::ExternalWebSocketClient { .. }
        | Action::CreateBridge { .. }
        | Action::Continue { .. } => true,
        Action::RecordChannel { .. }
        | Action::RecordBridge { .. }
        | Action::GetRecording(_)
        | Action::ControlRecording { .. }
        | Action::EndRecording { .. }
        | Action::PlayBridge { .. }
        | Action::PlayChannel { .. }
        | Action::GetPlayback(_)
        | Action::ControlPlayback { .. }
        | Action::StartMusic { .. }
        | Action::StopMusic(_)
        | Action::SendDtmf { .. }
        | Action::ListChannels
        | Action::GetChannel(_)
        | Action::ListApplications
        | Action::GetApplication(_)
        | Action::FilterApplication { .. }
        | Action::SubscribeResources { .. }
        | Action::UnsubscribeResources { .. }
        | Action::ListEndpoints { .. }
        | Action::GetEndpoint { .. }
        | Action::GetVariable { .. }
        | Action::SetVariable { .. }
        | Action::Hangup(_)
        | Action::Move { .. }
        | Action::Answer(_)
        | Action::Ring(_)
        | Action::Hold(_)
        | Action::Unhold(_)
        | Action::Mute { .. }
        | Action::Unmute { .. }
        | Action::ListBridges
        | Action::GetBridge(_)
        | Action::DestroyBridge(_)
        | Action::AddChannels { .. }
        | Action::RemoveChannels { .. } => false,
    }
}

pub(super) struct ReloadRequest {
    replacement: Config,
    reply: oneshot::Sender<Result<(), ControlError>>,
}

pub(super) struct DirectoryReloadRequest {
    credentials: Credentials,
    origin: Option<String>,
    reply: oneshot::Sender<Result<(), ControlError>>,
}

struct Services {
    native: NativePreparation,
    http: PublicationPort<PreparedHttpSettings>,
    signaling: PublicationPort<NativePreparedSignaling>,
    directory: Option<PathBuf>,
}

pub(super) struct PreparedReplacement {
    config: Config,
    settings: CallSettings,
    http: PreparedHttpSettings,
    signaling: NativePreparedSignaling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Loading,
    Preparing,
    Staging,
    Quiescing,
    Publishing,
    Resuming,
    Cancelling,
    Aborting,
    Draining,
}

struct Pending {
    reply: oneshot::Sender<Result<(), ControlError>>,
    key: Option<PublicationKey>,
    phase: Phase,
    deadline: Instant,
    barrier: bool,
    reserved: bool,
    failure: Option<ControlError>,
}

pub(super) struct Step {
    phase: Phase,
    result: Result<(), ControlError>,
}

pub(super) struct ReloadCoordinator {
    pub(super) requests: mpsc::Receiver<ReloadRequest>,
    pub(super) directory_requests: mpsc::Receiver<DirectoryReloadRequest>,
    pub(super) loaders: JoinSet<Result<Config, ConfigError>>,
    pub(super) preparations: JoinSet<Result<PreparedReplacement, ControlError>>,
    pub(super) steps: JoinSet<Step>,
    services: Option<Services>,
    pending: Option<Pending>,
}

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

pub(crate) fn load_startup_configuration(directory: &Path) -> Result<Config, ConfigError> {
    for (file, optional) in [
        ("http.conf", false),
        ("ari.conf", false),
        ("pjsip.conf", false),
        ("rtp.conf", false),
        ("extensions.conf", false),
        ("chan_websocket.conf", true),
        ("websocket_client.conf", true),
        ("musiconhold.conf", true),
    ] {
        let error = |reason: String| ConfigError {
            file: file.into(),
            line: 0,
            reason,
        };
        match std::fs::metadata(directory.join(file)) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => return Err(error("configuration input must be a regular file".into())),
            Err(issue) if optional && issue.kind() == std::io::ErrorKind::NotFound => {}
            Err(issue) => return Err(error(issue.to_string())),
        }
    }
    Config::load(directory)
}

fn combine(
    primary: Result<(), ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<(), ControlError> {
    match (primary, cleanup) {
        (primary, Ok(())) => primary,
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(primary), Err(cleanup)) => Err(failure(format!(
            "{primary}; settings publication cleanup failed: {cleanup}"
        ))),
    }
}

fn respond(reply: oneshot::Sender<Result<(), ControlError>>, result: Result<(), ControlError>) {
    if reply.send(result).is_err() {
        tracing::debug!("Settings requester disconnected");
    }
}

async fn acknowledgement(
    acknowledgement: Result<Acknowledgement, ControlError>,
    deadline: Instant,
) -> Result<(), ControlError> {
    tokio::time::timeout_at(deadline, acknowledgement?.receive())
        .await
        .map_err(failure)?
}

async fn acknowledged(
    first: Result<Acknowledgement, ControlError>,
    second: Result<Acknowledgement, ControlError>,
    deadline: Instant,
) -> Result<(), ControlError> {
    let (first, second) = tokio::join!(
        acknowledgement(first, deadline),
        acknowledgement(second, deadline)
    );
    combine(first, second)
}

async fn prepare(
    config: Config,
    native: NativePreparation,
    storage: StoragePort,
) -> Result<PreparedReplacement, ControlError> {
    let http = PreparedHttpSettings::prepare(&config.http).await?;
    let signaling = native.prepare(&config).await?;
    let music = storage
        .prepare_music(config.music_on_hold.clone())
        .await
        .map_err(failure)?;
    let settings = CallSettings::prepared(
        config.dialplan.clone().compile(),
        signaling.call_catalog(),
        signaling.websocket_catalog(),
        music,
    );
    Ok(PreparedReplacement {
        config,
        settings,
        http,
        signaling,
    })
}

impl ReloadCoordinator {
    pub(super) fn new(
        requests: mpsc::Receiver<ReloadRequest>,
        directory_requests: mpsc::Receiver<DirectoryReloadRequest>,
    ) -> Self {
        Self {
            requests,
            directory_requests,
            loaders: JoinSet::new(),
            preparations: JoinSet::new(),
            steps: JoinSet::new(),
            services: None,
            pending: None,
        }
    }

    fn pending(&mut self) -> Result<&mut Pending, ControlError> {
        self.pending
            .as_mut()
            .ok_or(ControlError::Conflict("Settings publication is missing"))
    }

    fn services(&self) -> Result<&Services, ControlError> {
        self.services.as_ref().ok_or(ControlError::Unsupported)
    }

    fn busy(&self) -> bool {
        self.pending.is_some()
            || !self.loaders.is_empty()
            || !self.preparations.is_empty()
            || !self.steps.is_empty()
    }

    fn begin_loading<F>(
        &mut self,
        directory: PathBuf,
        reply: oneshot::Sender<Result<(), ControlError>>,
        load: F,
    ) where
        F: FnOnce(&Path) -> Result<Config, ConfigError> + Send + 'static,
    {
        if reply.is_closed() {
            return;
        }
        if self.busy() {
            respond(reply, Err(ControlError::Capacity));
            return;
        }
        self.pending = Some(Pending {
            reply,
            key: None,
            phase: Phase::Loading,
            deadline: Instant::now() + PREPARATION_BUDGET,
            barrier: false,
            reserved: false,
            failure: None,
        });
        self.loaders.spawn_blocking(move || load(&directory));
    }

    fn step(&mut self, phase: Phase, command: PublicationPhase) -> Result<(), ControlError> {
        let pending = self.pending()?;
        let key = pending
            .key
            .ok_or(ControlError::Conflict("Settings key is missing"))?;
        let deadline = pending.deadline;
        pending.phase = phase;
        let services = self.services()?;
        let first = services.http.command(key, command);
        let second = services.signaling.command(key, command);
        self.steps.spawn(async move {
            Step {
                phase,
                result: acknowledged(first, second, deadline).await,
            }
        });
        Ok(())
    }
}

impl Controller {
    async fn reload_response(
        &self,
        receiver: oneshot::Receiver<Result<(), ControlError>>,
    ) -> Result<(), ControlError> {
        self.closures
            .answered(RELOAD_BUDGET, receiver)
            .await
            .map_err(failure)?
            .map_err(|_| ControlError::ServiceClosed)?
    }

    pub async fn reload(&self, replacement: Config) -> Result<(), ControlError> {
        if self.is_stopping() {
            return Err(ControlError::Conflict("Control is stopping"));
        }
        let (reply, receiver) = oneshot::channel();
        self.reload_requests
            .try_send(ReloadRequest { replacement, reply })
            .map_err(ControlError::queue)?;
        self.reload_response(receiver).await
    }

    pub(crate) async fn reload_startup(
        &self,
        credentials: Credentials,
        origin: Option<String>,
    ) -> Result<(), ControlError> {
        if self.is_stopping() {
            return Err(ControlError::Conflict("Control is stopping"));
        }
        let (reply, receiver) = oneshot::channel();
        self.directory_reload_requests
            .try_send(DirectoryReloadRequest {
                credentials,
                origin,
                reply,
            })
            .map_err(ControlError::queue)?;
        self.reload_response(receiver).await
    }
}

impl Runner {
    #[cfg(test)]
    pub(crate) fn begin_loading_for_shutdown_test<F>(
        &mut self,
        directory: PathBuf,
        reply: oneshot::Sender<Result<(), ControlError>>,
        primary: ControlError,
        load: F,
    ) where
        F: FnOnce(&Path) -> Result<Config, ConfigError> + Send + 'static,
    {
        self.reload.begin_loading(directory, reply, load);
        self.retain_reload_failure(&primary);
    }

    pub(crate) fn with_reload(
        self,
        native: NativePreparation,
        http: PublicationPort<PreparedHttpSettings>,
        signaling: PublicationPort<NativePreparedSignaling>,
    ) -> Self {
        self.with_reload_source(native, http, signaling, None)
    }

    pub(crate) fn with_reload_source(
        mut self,
        native: NativePreparation,
        http: PublicationPort<PreparedHttpSettings>,
        signaling: PublicationPort<NativePreparedSignaling>,
        directory: Option<PathBuf>,
    ) -> Self {
        self.reload.services = Some(Services {
            native,
            http,
            signaling,
            directory,
        });
        self
    }

    pub(crate) fn configuration_generation(&self) -> rusterisk_core::SettingsGeneration {
        self.control.configuration_generation()
    }

    pub(super) fn reload_wake(&self) -> Result<Wake, ControlError> {
        let Some(pending) = &self.reload.pending else {
            return Ok(Wake::Idle);
        };
        // A step ends at this deadline through its own timeout, and its result decides the outcome.
        if !self.reload.steps.is_empty() {
            return Ok(Wake::Idle);
        }
        let cleanup = matches!(
            pending.phase,
            Phase::Cancelling | Phase::Aborting | Phase::Draining
        );
        if cleanup && !self.reload.loaders.is_empty() {
            return Ok(Wake::Idle);
        }
        Ok(Wake::At(self.clock.at(pending.deadline)?))
    }

    pub(super) fn admissions_paused(&self) -> bool {
        self.reload
            .pending
            .as_ref()
            .is_some_and(|pending| pending.barrier)
    }

    pub(super) fn reject_new_transfer(
        &mut self,
        signal: &crate::Signal,
    ) -> Result<bool, ControlError> {
        if !self.admissions_paused() {
            return Ok(false);
        }
        let rejected = if let crate::Signal::BlindTransferRequested { key, refer } = signal {
            Some((key.clone(), refer.key()))
        } else if let crate::Signal::AttendedTransferRequested { key, refer, .. } = signal {
            Some((key.clone(), refer.key()))
        } else {
            None
        };
        let Some((source, refer)) = rejected else {
            return Ok(false);
        };
        let mut outcome = crate::Outcome::empty();
        outcome.effects.push(crate::Effect::RejectRefer {
            source,
            refer,
            failure: rusterisk_sip::ReferFailure::Unavailable,
        });
        self.publish(&mut outcome)?;
        Ok(true)
    }

    pub(super) fn begin_reload(&mut self, request: ReloadRequest) {
        if request.reply.is_closed() {
            return;
        }
        let replacement = request.replacement;
        let result = self.check_replacement(&replacement);
        let (native, storage) = match result {
            Ok(services) => services,
            Err(error) => return respond(request.reply, Err(error)),
        };
        self.reload.pending = Some(Pending {
            reply: request.reply,
            key: None,
            phase: Phase::Preparing,
            deadline: Instant::now() + PREPARATION_BUDGET,
            barrier: false,
            reserved: false,
            failure: None,
        });
        self.reload
            .preparations
            .spawn(prepare(replacement, native, storage));
    }

    pub(super) fn begin_directory_reload(&mut self, request: DirectoryReloadRequest) {
        if request.reply.is_closed() {
            return;
        }
        if let Err(error) = self.authorize(&request.credentials, request.origin.as_deref(), false) {
            return respond(request.reply, Err(error));
        }
        let directory = match self
            .reload
            .services
            .as_ref()
            .and_then(|services| services.directory.clone())
        {
            Some(directory) => directory,
            None => return respond(request.reply, Err(ControlError::Unsupported)),
        };
        self.reload
            .begin_loading(directory, request.reply, load_startup_configuration);
    }

    fn check_replacement(
        &self,
        replacement: &Config,
    ) -> Result<(NativePreparation, StoragePort), ControlError> {
        if self.reload.busy() {
            return Err(ControlError::Capacity);
        }
        self.preparation_services(replacement)
    }

    fn preparation_services(
        &self,
        replacement: &Config,
    ) -> Result<(NativePreparation, StoragePort), ControlError> {
        replacement
            .validate_runtime()
            .map_err(ControlError::Configuration)?;
        if matches!(self.config.reload(replacement), Reload::RestartRequired) {
            return Err(ControlError::Conflict(
                "Listener replacement requires restart",
            ));
        }
        let native = self.reload.services()?.native.clone();
        let storage = self
            .storage
            .as_ref()
            .ok_or(ControlError::Unsupported)?
            .port
            .clone();
        Ok((native, storage))
    }

    pub(super) fn loading_completed(
        &mut self,
        result: Result<Result<Config, ConfigError>, JoinError>,
    ) -> Result<(), ControlError> {
        let pending = self.reload.pending()?;
        if pending.phase == Phase::Cancelling {
            let cleanup = match result {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(error)) => Err(ControlError::Configuration(error)),
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(failure(error)),
            };
            pending.failure = combine(pending.failure.take().map_or(Ok(()), Err), cleanup).err();
            return self.finish_aborted_reload();
        }
        if pending.phase != Phase::Loading {
            return Err(ControlError::Conflict("Unexpected settings load"));
        }
        let result = result
            .map_err(failure)
            .and_then(|result| result.map_err(ControlError::Configuration));
        if pending.reply.is_closed() || Instant::now() >= pending.deadline {
            let cancelled = match result {
                Ok(_) => ControlError::Conflict("Settings loading cancelled"),
                Err(error) => failure(format!("Settings loading cancelled; {error}")),
            };
            return self.abort_reload(cancelled);
        }
        let config = match result {
            Ok(config) => config,
            Err(error) => return self.abort_reload(error),
        };
        let (native, storage) = match self.preparation_services(&config) {
            Ok(services) => services,
            Err(error) => return self.abort_reload(error),
        };
        self.reload.pending()?.phase = Phase::Preparing;
        self.reload
            .preparations
            .spawn(prepare(config, native, storage));
        Ok(())
    }

    pub(super) fn preparation_completed(
        &mut self,
        result: Result<Result<PreparedReplacement, ControlError>, JoinError>,
    ) -> Result<(), ControlError> {
        let pending = self.reload.pending()?;
        if pending.phase == Phase::Cancelling {
            let cleanup = match result {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(error)) => Err(error),
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(failure(error)),
            };
            pending.failure = combine(pending.failure.take().map_or(Ok(()), Err), cleanup).err();
            return self.finish_aborted_reload();
        }
        if pending.phase != Phase::Preparing {
            return Err(ControlError::Conflict("Unexpected settings preparation"));
        }
        if pending.reply.is_closed() || Instant::now() >= pending.deadline {
            return self.abort_reload(ControlError::Conflict("Settings preparation cancelled"));
        }
        let prepared = match result.map_err(failure).and_then(|result| result) {
            Ok(prepared) => prepared,
            Err(error) => return self.abort_reload(error),
        };
        self.stage_replacement(prepared)
    }

    fn stage_replacement(&mut self, prepared: PreparedReplacement) -> Result<(), ControlError> {
        let base = self.control.configuration_generation();
        let candidate = match self
            .control
            .prepare_configuration(prepared.config, prepared.settings)
        {
            Ok(candidate) => candidate,
            Err(error) => return self.abort_reload(error),
        };
        let key = PublicationKey::new(candidate, base);
        let pending = self.reload.pending()?;
        pending.key = Some(key);
        pending.reserved = true;
        pending.phase = Phase::Staging;
        let deadline = pending.deadline;
        let services = self.reload.services()?;
        let first = services.http.stage(key, prepared.http);
        let second = services.signaling.stage(key, prepared.signaling);
        self.reload.steps.spawn(async move {
            Step {
                phase: Phase::Staging,
                result: acknowledged(first, second, deadline).await,
            }
        });
        Ok(())
    }

    pub(super) fn publication_completed(
        &mut self,
        result: Result<Step, JoinError>,
    ) -> Result<(), ControlError> {
        let pending = self.reload.pending()?;
        if pending.phase == Phase::Cancelling {
            let cleanup = match result {
                Ok(step) => step.result,
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(failure(error)),
            };
            pending.failure = combine(pending.failure.take().map_or(Ok(()), Err), cleanup).err();
            return self.finish_aborted_reload();
        }
        let step = result.map_err(failure)?;
        if pending.phase != step.phase {
            return Err(ControlError::Conflict(
                "Settings acknowledgement is out of order",
            ));
        }
        if matches!(step.phase, Phase::Publishing | Phase::Resuming)
            && Instant::now() >= pending.deadline
        {
            return Err(ControlError::Conflict("Settings publication timed out"));
        }
        if let Err(error) = step.result {
            if matches!(
                step.phase,
                Phase::Publishing | Phase::Resuming | Phase::Aborting
            ) {
                return Err(error);
            }
            return self.abort_reload(error);
        }
        match step.phase {
            Phase::Staging => self.quiesce_replacement(),
            Phase::Quiescing => self.commit_replacement(),
            Phase::Publishing => {
                let generation = self.control.configuration_generation();
                self.reload
                    .step(Phase::Resuming, PublicationPhase::Resume(generation))
            }
            Phase::Resuming => self.finish_replacement(),
            Phase::Aborting => {
                self.reload.pending()?.phase = Phase::Draining;
                self.finish_aborted_reload()
            }
            Phase::Loading | Phase::Preparing | Phase::Cancelling | Phase::Draining => Err(
                ControlError::Conflict("Unexpected settings acknowledgement"),
            ),
        }
    }

    fn quiesce_replacement(&mut self) -> Result<(), ControlError> {
        let pending = self.reload.pending()?;
        if pending.reply.is_closed() || Instant::now() >= pending.deadline {
            return self.abort_reload(ControlError::Conflict("Settings requester disconnected"));
        }
        pending.barrier = true;
        pending.deadline = Instant::now() + PUBLICATION_BUDGET;
        self.reload
            .step(Phase::Quiescing, PublicationPhase::Quiesce)
    }

    fn commit_replacement(&mut self) -> Result<(), ControlError> {
        let pending = self.reload.pending()?;
        if pending.reply.is_closed() || Instant::now() >= pending.deadline {
            return self.abort_reload(ControlError::Conflict("Settings publication cancelled"));
        }
        let candidate = pending
            .key
            .ok_or(ControlError::Conflict("Settings key is missing"))?
            .candidate();
        let config = match self.control.commit_configuration(candidate) {
            Ok(config) => config,
            Err(error) => return self.abort_reload(error),
        };
        self.config = config;
        self.reload.pending()?.reserved = false;
        let generation = self.control.configuration_generation();
        self.reload
            .step(Phase::Publishing, PublicationPhase::Publish(generation))
    }

    fn finish_replacement(&mut self) -> Result<(), ControlError> {
        let pending = self
            .reload
            .pending
            .take()
            .ok_or(ControlError::Conflict("Settings publication is missing"))?;
        self.control.configuration_published(super::timestamp());
        respond(pending.reply, Ok(()));
        Ok(())
    }

    fn abort_reload(&mut self, primary: ControlError) -> Result<(), ControlError> {
        let pending = self.reload.pending()?;
        if matches!(pending.phase, Phase::Publishing | Phase::Resuming) {
            return Err(primary);
        }
        pending.failure = combine(pending.failure.take().map_or(Ok(()), Err), Err(primary)).err();
        pending.deadline = Instant::now() + PUBLICATION_BUDGET;
        let key = pending.key;
        if pending.reserved {
            self.control.cancel_configuration(
                key.ok_or(ControlError::Conflict("Settings key is missing"))?
                    .candidate(),
            )?;
            self.reload.pending()?.reserved = false;
        }
        self.reload.pending()?.phase = Phase::Cancelling;
        self.reload.loaders.abort_all();
        self.reload.preparations.abort_all();
        self.reload.steps.abort_all();
        self.finish_aborted_reload()
    }

    fn finish_aborted_reload(&mut self) -> Result<(), ControlError> {
        if !self.reload.loaders.is_empty()
            || !self.reload.preparations.is_empty()
            || !self.reload.steps.is_empty()
        {
            return Ok(());
        }
        let pending = self.reload.pending()?;
        if pending.phase == Phase::Cancelling && pending.key.is_some() {
            return self.reload.step(Phase::Aborting, PublicationPhase::Abort);
        }
        if !matches!(pending.phase, Phase::Cancelling | Phase::Draining) {
            return Ok(());
        }
        let pending = self
            .reload
            .pending
            .take()
            .ok_or(ControlError::Conflict("Settings publication is missing"))?;
        respond(
            pending.reply,
            Err(pending
                .failure
                .ok_or(ControlError::Conflict("Settings failure is missing"))?),
        );
        Ok(())
    }

    pub(super) fn tick_reload(&mut self) -> Result<(), ControlError> {
        let Some(pending) = &self.reload.pending else {
            return Ok(());
        };
        if matches!(pending.phase, Phase::Publishing | Phase::Resuming) {
            if Instant::now() >= pending.deadline {
                return Err(ControlError::Conflict("Settings publication timed out"));
            }
            return Ok(());
        }
        if matches!(
            pending.phase,
            Phase::Cancelling | Phase::Aborting | Phase::Draining
        ) {
            if Instant::now() >= pending.deadline && self.reload.loaders.is_empty() {
                return Err(ControlError::Conflict("Settings cleanup timed out"));
            }
            return self.finish_aborted_reload();
        }
        if pending.reply.is_closed() || Instant::now() >= pending.deadline {
            self.abort_reload(ControlError::Conflict("Settings preparation cancelled"))?;
        }
        Ok(())
    }

    pub(super) fn retain_reload_failure(&mut self, error: &ControlError) {
        if let Some(pending) = &mut self.reload.pending {
            pending.failure = combine(
                pending.failure.take().map_or(Ok(()), Err),
                Err(error.clone()),
            )
            .err();
        }
    }

    pub(super) async fn close_reload(&mut self, deadline: Instant) -> Result<(), ControlError> {
        self.reload.requests.close();
        self.reload.directory_requests.close();
        self.reload.loaders.abort_all();
        self.reload.preparations.abort_all();
        self.reload.steps.abort_all();
        let mut result = Ok(());
        while let Some(joined) = self.reload.loaders.join_next().await {
            match joined {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => result = combine(result, Err(ControlError::Configuration(error))),
                Err(error) if error.is_cancelled() => {}
                Err(error) => result = combine(result, Err(failure(error))),
            }
        }
        while let Some(joined) = self.reload.preparations.join_next().await {
            match joined {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => result = combine(result, Err(error)),
                Err(error) if error.is_cancelled() => {}
                Err(error) => result = combine(result, Err(failure(error))),
            }
        }
        while let Some(joined) = self.reload.steps.join_next().await {
            match joined {
                Ok(step) => result = combine(result, step.result),
                Err(error) if error.is_cancelled() => {}
                Err(error) => result = combine(result, Err(failure(error))),
            }
        }
        if let Some(pending) = self.reload.pending.take() {
            if pending.reserved {
                if let Some(key) = pending.key {
                    result = combine(result, self.control.cancel_configuration(key.candidate()));
                }
            }
            let primary = pending
                .failure
                .unwrap_or(ControlError::Conflict("Control is stopping"));
            respond(pending.reply, combine(Err(primary), result.clone()));
        }
        if Instant::now() >= deadline {
            result = combine(
                result,
                Err(ControlError::Conflict("Settings cleanup timed out")),
            );
        }
        result
    }
}
