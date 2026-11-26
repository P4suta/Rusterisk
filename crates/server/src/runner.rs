mod calls;
use crate::clock::Clock;
use crate::storage::{StoragePort, WorkerError};
use crate::{Action, CallKey, Control, ControlError, Effect, IncomingCall, Outcome, Signal};
use rusterisk_api::{Event, EventKind};
use rusterisk_config::Config;
use rusterisk_core::Wake;
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;
mod answer;
mod call_lifetime;
#[cfg(test)]
mod clock_tests;
mod closure;
use closure::{ClosureLatch, ClosureNotice};
#[cfg(test)]
mod composed_tests;
#[cfg(test)]
mod deadline_tests;
mod diagnosis;
mod endpoint_queries;
#[cfg(test)]
mod endpoint_queries_tests;
mod external_admission;
mod history;
mod input;
#[cfg(test)]
mod turn_capacity_tests;
pub(crate) use input::Flow;
mod media_connections;
mod observation;
pub(crate) use observation::{Filter as WatchFilter, Target as WatchTarget};
mod native_start;
mod observed_channel;

pub(crate) use diagnosis::Target as DiagnosisTarget;
mod recording_admission;
mod reload;
pub(crate) use reload::load_startup_configuration;
mod stored_recordings;
#[cfg(test)]
mod transfer_tests;
mod websocket_profiles;
pub(crate) use call_lifetime::CallLease;
use history::HistoryRecorder;
pub use media_connections::{ClaimedMediaConnection, MediaConnectionLease};
pub(crate) use stored_recordings::Operation as StoredRecordingOperation;
pub(crate) use stored_recordings::Response as StoredRecordingResponse;
mod sounds;
pub(crate) use sounds::Query as SoundQuery;

struct StorageServices {
    port: StoragePort,
    history: HistoryRecorder,
}

// Ordinary work may queue this many effects in one turn.
const TURN_EFFECTS: usize = 128;
// Terminal cleanup of every call fits behind a turn that ordinary work filled.
const MAX_PENDING_EFFECTS: usize = TURN_EFFECTS + 64 * 3 + 1;

// Where control effects go: through a channel to a separate signaling task, or to the turn that composes both.
enum Outlet {
    Channel(mpsc::Sender<Effect>),
    Turn,
}
pub(crate) const OVERDUE_RETRY_MS: u64 = 100;

enum Admission {
    Recording {
        owner: crate::MediaOwner,
        token: rusterisk_core::MediaTaskToken,
    },
    ExternalMedia(CallKey),
}

#[derive(Clone)]
pub struct Credentials {
    username: String,
    password: String,
}

impl Credentials {
    pub fn new(username: String, password: String) -> Result<Self, ControlError> {
        if username.is_empty()
            || username.len() > 128
            || password.is_empty()
            || password.len() > 256
        {
            return Err(ControlError::Unauthorized);
        }
        Ok(Self { username, password })
    }
}

enum Request {
    ObserveCalls {
        credentials: Credentials,
        origin: Option<String>,
        target: WatchTarget,
        reply: oneshot::Sender<Result<observation::Observation, ControlError>>,
    },
    Diagnose {
        credentials: Credentials,
        origin: Option<String>,
        target: DiagnosisTarget,
        reply: oneshot::Sender<Result<serde_json::Value, ControlError>>,
    },
    ObservedChannel {
        credentials: Credentials,
        origin: Option<String>,
        channel: rusterisk_api::ChannelId,
        reply: oneshot::Sender<Result<(CallKey, String), ControlError>>,
    },
    Answer {
        credentials: Credentials,
        origin: Option<String>,
        channel: rusterisk_api::ChannelId,
        reply: oneshot::Sender<Result<Outcome, ControlError>>,
    },
    Info {
        only: Option<String>,
        reply: oneshot::Sender<Result<rusterisk_api::AsteriskInfo, ControlError>>,
    },
    Sounds {
        credentials: Credentials,
        origin: Option<String>,
        query: SoundQuery,
        reply: oneshot::Sender<Result<Outcome, ControlError>>,
    },
    CallLifetime {
        key: CallKey,
        lifetime: mpsc::Receiver<()>,
        reply: oneshot::Sender<Result<(), ControlError>>,
    },
    ClaimMediaConnection {
        connection_id: rusterisk_api::ChannelId,
        origin: Option<String>,
        lifetime: mpsc::Receiver<()>,
        reply: oneshot::Sender<
            Result<(CallKey, crate::media_websocket::MediaLinkOptions), ControlError>,
        >,
    },
    StoredRecording {
        credentials: Credentials,
        origin: Option<String>,
        operation: StoredRecordingOperation,
        reply: oneshot::Sender<Result<StoredRecordingResponse, ControlError>>,
    },
    Incoming {
        call: IncomingCall,
        reply: oneshot::Sender<Result<CallKey, ControlError>>,
    },
    Authorize {
        credentials: Credentials,
        origin: Option<String>,
        read_only: bool,
        reply: oneshot::Sender<Result<(), ControlError>>,
    },
    Apply {
        credentials: Credentials,
        origin: Option<String>,
        action: Action,
        variables: BTreeMap<String, String>,
        reply: oneshot::Sender<Result<Outcome, ControlError>>,
    },
    Subscribe {
        credentials: Credentials,
        origin: Option<String>,
        application: String,
        events: mpsc::Sender<Event>,
        lifetime: mpsc::Receiver<()>,
        kind: SubscriptionKind,
        reply: oneshot::Sender<Result<u64, ControlError>>,
    },
}

struct Subscriber {
    events: mpsc::Sender<Event>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SubscriptionKind {
    Direct,
    Socket,
}
struct Session {
    lifetime: mpsc::Receiver<()>,
    kind: SubscriptionKind,
}

#[derive(Clone)]
pub struct Controller {
    pub(crate) activity: crate::activity::Meters,
    clock: Clock,
    closures: ClosureLatch,
    requests: mpsc::Sender<Request>,
    signals: mpsc::Sender<Signal>,
    shutdown: watch::Sender<bool>,
    reload_requests: mpsc::Sender<reload::ReloadRequest>,
    directory_reload_requests: mpsc::Sender<reload::DirectoryReloadRequest>,
}

pub struct Runner {
    pub(crate) activity: crate::activity::Meters,
    records: crate::call_records::CallRecords<calls::Resources>,
    clock: Clock,
    closed: mpsc::Receiver<()>,
    closures_open: bool,
    observations: tokio::sync::broadcast::Sender<rusterisk_api::observation::WatchRecord>,
    control: Control,
    config: Config,
    requests: mpsc::Receiver<Request>,
    signals: mpsc::Receiver<Signal>,
    effects: Outlet,
    pending_effects: VecDeque<Effect>,
    shutdown: watch::Receiver<bool>,
    stop: watch::Sender<bool>,
    reload: reload::ReloadCoordinator,
    subscribers: BTreeMap<String, Subscriber>,
    sessions: BTreeMap<u64, Session>,
    subscription_generation: u64,
    storage: Option<StorageServices>,
    history_jobs: JoinSet<Result<(), WorkerError>>,
    stored_jobs: JoinSet<()>,
    recording_admissions: BTreeMap<rusterisk_core::MediaTaskToken, recording_admission::Pending>,
}

pub struct Subscription {
    pub application: String,
    pub token: u64,
    pub events: mpsc::Receiver<Event>,
    _controller: Controller,
    _lifetime: mpsc::Sender<()>,
    _closure: ClosureNotice,
}

impl Controller {
    async fn receive<T>(
        &self,
        receiver: oneshot::Receiver<Result<T, ControlError>>,
    ) -> Result<T, ControlError> {
        self.closures
            .answered(Duration::from_secs(5), receiver)
            .await
            .map_err(|_| ControlError::Capacity)?
            .map_err(|_| ControlError::ServiceClosed)?
    }

    pub async fn incoming(&self, call: IncomingCall) -> Result<CallKey, ControlError> {
        let (reply, receiver) = oneshot::channel();
        self.requests
            .try_send(Request::Incoming { call, reply })
            .map_err(ControlError::queue)?;
        self.receive(receiver).await
    }

    pub fn new(config: Config, capacity: usize) -> (Self, Runner, mpsc::Receiver<Effect>) {
        let (requests, request_receiver) = mpsc::channel(128);
        let (signals, signal_receiver) = mpsc::channel(128);
        let (effects, effect_receiver) = mpsc::channel(128);
        let (shutdown, shutdown_receiver) = watch::channel(false);
        let (reload_requests, reload_receiver) = mpsc::channel(1);
        let (directory_reload_requests, directory_reload_receiver) = mpsc::channel(1);
        let control = Control::with_configuration(capacity, &config);
        let clock = Clock::start();
        let (closures, closed) = closure::latch();
        let activity = crate::activity::Meters::default();
        (
            Self {
                activity: activity.clone(),
                clock,
                closures,
                requests,
                signals,
                shutdown: shutdown.clone(),
                reload_requests,
                directory_reload_requests,
            },
            Runner {
                activity,
                clock,
                closed,
                closures_open: true,
                control,
                config,
                requests: request_receiver,
                signals: signal_receiver,
                effects: Outlet::Channel(effects),
                pending_effects: VecDeque::with_capacity(MAX_PENDING_EFFECTS),
                shutdown: shutdown_receiver,
                stop: shutdown,
                reload: reload::ReloadCoordinator::new(reload_receiver, directory_reload_receiver),
                subscribers: BTreeMap::new(),
                observations: tokio::sync::broadcast::channel(128).0,
                sessions: BTreeMap::new(),
                subscription_generation: 0,
                storage: None,
                history_jobs: JoinSet::new(),
                stored_jobs: JoinSet::new(),
                recording_admissions: BTreeMap::new(),
                records: crate::call_records::CallRecords::new(),
            },
            effect_receiver,
        )
    }

    pub(crate) fn clock(&self) -> Clock {
        self.clock
    }

    pub fn shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    pub(crate) async fn server_info(
        &self,
        only: Option<&str>,
    ) -> Result<rusterisk_api::AsteriskInfo, ControlError> {
        let (reply, receiver) = oneshot::channel();
        self.requests
            .try_send(Request::Info {
                only: only.map(str::to_owned),
                reply,
            })
            .map_err(ControlError::queue)?;
        self.receive(receiver).await
    }

    pub fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    pub fn is_stopping(&self) -> bool {
        *self.shutdown.borrow()
    }

    pub async fn signal(&self, signal: Signal) -> Result<(), ControlError> {
        self.signals.try_send(signal).map_err(ControlError::queue)
    }

    pub async fn authorize(
        &self,
        credentials: Credentials,
        origin: Option<String>,
        read_only: bool,
    ) -> Result<(), ControlError> {
        let (reply, receiver) = oneshot::channel();
        self.requests
            .try_send(Request::Authorize {
                credentials,
                origin,
                read_only,
                reply,
            })
            .map_err(ControlError::queue)?;
        self.receive(receiver).await
    }

    pub async fn execute(
        &self,
        credentials: Credentials,
        origin: Option<String>,
        action: Action,
    ) -> Result<Outcome, ControlError> {
        self.execute_with_variables(credentials, origin, action, BTreeMap::new())
            .await
    }

    pub(crate) async fn execute_with_variables(
        &self,
        credentials: Credentials,
        origin: Option<String>,
        action: Action,
        variables: BTreeMap<String, String>,
    ) -> Result<Outcome, ControlError> {
        let (reply, receiver) = oneshot::channel();
        self.requests
            .try_send(Request::Apply {
                credentials,
                origin,
                action,
                variables,
                reply,
            })
            .map_err(ControlError::queue)?;
        self.receive(receiver).await
    }

    pub async fn subscribe(
        &self,
        credentials: Credentials,
        origin: Option<String>,
        application: String,
    ) -> Result<Subscription, ControlError> {
        self.subscribe_kind(credentials, origin, application, SubscriptionKind::Direct)
            .await
    }

    pub(crate) async fn subscribe_socket(
        &self,
        credentials: Credentials,
        origin: Option<String>,
        application: String,
    ) -> Result<Subscription, ControlError> {
        self.subscribe_kind(credentials, origin, application, SubscriptionKind::Socket)
            .await
    }

    async fn subscribe_kind(
        &self,
        credentials: Credentials,
        origin: Option<String>,
        application: String,
        kind: SubscriptionKind,
    ) -> Result<Subscription, ControlError> {
        let closure = self.closures.notice();
        let (events, receiver) = mpsc::channel(64);
        let (lifetime, lifetime_receiver) = mpsc::channel(1);
        let (reply, response) = oneshot::channel();
        self.requests
            .try_send(Request::Subscribe {
                credentials,
                origin,
                application: application.clone(),
                events,
                lifetime: lifetime_receiver,
                kind,
                reply,
            })
            .map_err(ControlError::queue)?;
        let token = self.receive(response).await?;
        Ok(Subscription {
            application,
            token,
            events: receiver,
            _controller: self.clone(),
            _lifetime: lifetime,
            _closure: closure,
        })
    }
}

fn timestamp() -> String {
    crate::utc_timestamps::now_millis()
}

impl Runner {
    pub(crate) fn with_native_profiles(
        mut self,
        profiles: crate::call_profiles::BoundNativeProfiles,
    ) -> Result<Self, ControlError> {
        self.control.bind_native_profiles(profiles)?;
        Ok(self)
    }

    pub(crate) fn with_music(
        mut self,
        music: crate::storage::PreparedMusic,
    ) -> Result<Self, ControlError> {
        self.control = self.control.with_music(music)?;
        Ok(self)
    }

    pub fn with_storage(mut self, storage: StoragePort) -> Result<Self, ControlError> {
        self.storage = Some(StorageServices {
            history: HistoryRecorder::new(storage.clone())?,
            port: storage,
        });
        Ok(self)
    }

    fn publish_history(&mut self, outcome: &mut Outcome) -> Result<(), ControlError> {
        if outcome.history.is_empty() {
            return Ok(());
        }
        match &self.storage {
            Some(storage) => {
                if self.history_jobs.len() >= 64 {
                    return Err(ControlError::Capacity);
                }
                let receipt = storage.history.queue(&mut outcome.history)?;
                self.history_jobs.spawn(receipt.wait());
            }
            None => outcome.history.clear(),
        }
        Ok(())
    }

    fn authorize(
        &self,
        credentials: &Credentials,
        origin: Option<&str>,
        read_only: bool,
    ) -> Result<(), ControlError> {
        if origin.is_some_and(|origin| !self.config.origins.iter().any(|allowed| origin == allowed))
        {
            return Err(ControlError::Forbidden);
        }
        let user = self
            .config
            .users
            .iter()
            .find(|user| user.name == credentials.username)
            .ok_or(ControlError::Unauthorized)?;
        if !bool::from(
            user.password
                .expose()
                .as_bytes()
                .ct_eq(credentials.password.as_bytes()),
        ) {
            return Err(ControlError::Unauthorized);
        }
        if user.read_only && !read_only {
            return Err(ControlError::Forbidden);
        }
        Ok(())
    }

    // The effects that ordinary work can still queue in this turn.
    fn effect_room(&self) -> usize {
        TURN_EFFECTS.saturating_sub(self.pending_effects.len())
    }

    // Whether this turn's effects still fit the ordinary tier, so that none waits in the terminal reserve.
    fn effects_flowing(&self) -> bool {
        self.pending_effects.len() <= TURN_EFFECTS
    }

    #[cfg(test)]
    pub(crate) fn effect_sender(&self) -> Result<&mpsc::Sender<Effect>, ControlError> {
        match &self.effects {
            Outlet::Channel(sender) => Ok(sender),
            Outlet::Turn => Err(ControlError::Conflict(
                "A composed control task has no effect channel",
            )),
        }
    }

    // Sends effects to the turn that composes signaling and call decisions.
    pub(crate) fn compose(mut self) -> Self {
        self.effects = Outlet::Turn;
        self
    }

    // Refuses every request that waits now, and every later one.
    pub(crate) fn refuse_requests(&mut self) {
        self.requests.close();
        while let Ok(request) = self.requests.try_recv() {
            drop(request);
        }
    }

    // Takes the next effect that the composing turn applies.
    pub(crate) fn take_effect(&mut self) -> Option<Effect> {
        self.pending_effects.pop_front()
    }

    // Marks a failed composed task, as a failed control task marks itself.
    pub(crate) fn fail(&mut self, error: &ControlError) {
        self.retain_reload_failure(error);
        self.stop.send_replace(true);
    }

    fn emit_effect(&mut self, effect: Effect) -> Result<(), ControlError> {
        if let Effect::Release { key } = &effect {
            self.records.remove(calls::LIFETIME, key);
        }
        let effect = match &self.effects {
            Outlet::Channel(sender) if self.pending_effects.is_empty() => {
                match sender.try_send(effect) {
                    Ok(()) => return Ok(()),
                    Err(mpsc::error::TrySendError::Full(effect)) => effect,
                    Err(mpsc::error::TrySendError::Closed(_)) if *self.shutdown.borrow() => {
                        return Ok(())
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        return Err(ControlError::Adapter(
                            "Signaling effect receiver closed".into(),
                        ))
                    }
                }
            }
            Outlet::Channel(_) | Outlet::Turn => effect,
        };
        if self.pending_effects.len() >= MAX_PENDING_EFFECTS {
            return Err(ControlError::Capacity);
        }
        self.pending_effects.push_back(effect);
        Ok(())
    }

    fn publish(&mut self, outcome: &mut Outcome) -> Result<(), ControlError> {
        self.publish_observations(outcome);
        self.control.route_resource_events(outcome);
        self.publish_history(outcome)?;
        for effect in outcome.effects.drain(..) {
            self.emit_effect(effect)?;
        }
        let mut disconnected = Vec::new();
        for event in outcome.events.drain(..) {
            if !self.control.event_permitted(&event) {
                continue;
            }
            if let Some(subscriber) = self.subscribers.get(&event.application) {
                let application = event.application.clone();
                if subscriber.events.try_send(event).is_err() {
                    self.activity.control.dropped();
                    if !disconnected.contains(&application) {
                        disconnected.push(application);
                    }
                }
            }
        }
        for application in disconnected {
            self.subscribers.remove(&application);
            let mut cleanup = self.control.unsubscribe(&application, &timestamp())?;
            self.publish_observations(&cleanup);
            self.publish_history(&mut cleanup)?;
            for effect in cleanup.effects.drain(..) {
                self.emit_effect(effect)?;
            }
        }
        Ok(())
    }

    // Admits an incoming call and publishes its outcome.
    // The outer error ends the control task, and the inner error refuses the call.
    pub(crate) fn incoming_call(
        &mut self,
        call: IncomingCall,
        now_ms: u64,
    ) -> Result<Result<CallKey, ControlError>, ControlError> {
        if self.admissions_paused() || !self.effects_flowing() {
            return Ok(Err(ControlError::Capacity));
        }
        match self.admit_incoming(call, now_ms) {
            Ok((key, mut outcome)) => {
                self.publish(&mut outcome)?;
                Ok(Ok(key))
            }
            Err(error) => Ok(Err(error)),
        }
    }

    fn request(&mut self, request: Request, now_ms: u64) -> Result<(), ControlError> {
        match request {
            Request::ObserveCalls {
                credentials,
                origin,
                target,
                reply,
            } => {
                if reply
                    .send(self.observe_calls(&credentials, origin.as_deref(), target))
                    .is_err()
                {
                    tracing::debug!("Call observer disconnected during setup");
                }
            }
            Request::Diagnose {
                credentials,
                origin,
                target,
                reply,
            } => {
                let result = self
                    .authorize(&credentials, origin.as_deref(), true)
                    .and_then(|()| self.diagnose(target));
                if reply.send(result).is_err() {
                    tracing::debug!("Diagnosis requester disconnected");
                }
            }
            Request::ObservedChannel {
                credentials,
                origin,
                channel,
                reply,
            } => {
                let result = self
                    .authorize(&credentials, origin.as_deref(), true)
                    .and_then(|()| {
                        Ok((
                            self.control.key(&channel)?,
                            self.control.channel(&channel)?.creationtime.clone(),
                        ))
                    });
                if reply.send(result).is_err() {
                    tracing::debug!("Channel observer disconnected");
                }
            }
            Request::Answer {
                credentials,
                origin,
                channel,
                reply,
            } => {
                self.request_answer(credentials, origin, channel, reply, now_ms);
            }
            Request::Info { only, reply } => {
                let selected = self.control.server_info().selected(only.as_deref());
                if reply.send(Ok(selected)).is_err() {
                    tracing::debug!("Server info requester disconnected");
                }
            }
            Request::Sounds {
                credentials,
                origin,
                query,
                reply,
            } => {
                self.sound_request(credentials, origin, query, reply);
            }
            Request::CallLifetime {
                key,
                lifetime,
                reply,
            } => self.claim_call_lifetime(key, lifetime, reply),
            Request::ClaimMediaConnection {
                connection_id,
                origin,
                lifetime,
                reply,
            } => self.claim_media_request(connection_id, origin, lifetime, reply),
            Request::StoredRecording {
                credentials,
                origin,
                operation,
                reply,
            } => {
                self.stored_request(credentials, origin, operation, reply);
            }
            Request::Incoming { call, reply } => {
                if reply.is_closed() {
                    return Ok(());
                }
                let result = match self.incoming_call(call, now_ms) {
                    Ok(result) => result,
                    Err(error) => {
                        if reply.send(Err(error.clone())).is_err() {
                            tracing::debug!(
                                "incoming admission requester disconnected during overload"
                            );
                        }
                        return Err(error);
                    }
                };
                if let Err(Ok(key)) = reply.send(result) {
                    if self
                        .control
                        .key(&key.id)
                        .is_ok_and(|current| current == key)
                    {
                        let mut cleanup =
                            self.control
                                .apply(Action::Hangup(key.id), &timestamp(), now_ms)?;
                        self.publish(&mut cleanup)?;
                    }
                }
            }
            Request::Authorize {
                credentials,
                origin,
                read_only,
                reply,
            } => {
                if reply
                    .send(self.authorize(&credentials, origin.as_deref(), read_only))
                    .is_err()
                {
                    tracing::debug!("authorization requester disconnected");
                }
            }
            Request::Apply {
                credentials,
                origin,
                action,
                variables,
                reply,
            } => {
                if reply.is_closed() {
                    return Ok(());
                }
                let recording = matches!(
                    &action,
                    Action::RecordChannel { .. } | Action::RecordBridge { .. }
                );
                let external = matches!(
                    &action,
                    Action::ExternalRtp { .. }
                        | Action::ExternalAudioSocket { .. }
                        | Action::ExternalWebSocketClient { .. }
                );
                let result = self
                    .authorize(&credentials, origin.as_deref(), action.read_only())
                    .and_then(|()| {
                        if (!self.effects_flowing() && !action.read_only())
                            || (self.admissions_paused() && reload::new_settings(&action))
                            || self.effect_room() < self.control.effect_budget(&action)
                            || (recording
                                && (self.recording_admissions.len() >= 64
                                    || now_ms.checked_add(5000).is_none()))
                            || (external
                                && (self.records.iter(calls::ADMISSION).count() >= 64
                                    || now_ms.checked_add(5000).is_none()))
                        {
                            return Err(ControlError::Capacity);
                        }
                        self.control
                            .apply_with_variables(action, variables, &timestamp(), now_ms)
                    });
                let result = match result {
                    Ok(mut outcome) => {
                        let admission = outcome.effects.iter().find_map(|effect| match effect {
                            Effect::RecordMedia { owner, token, .. } => {
                                Some(Admission::Recording {
                                    owner: owner.clone(),
                                    token: *token,
                                })
                            }
                            Effect::RejectRefer { .. }
                            | Effect::ReferProgress { .. }
                            | Effect::ControlRecording { .. }
                            | Effect::ReserveWebSocket { .. }
                            | Effect::EndRecording { .. }
                            | Effect::PlayMedia { .. }
                            | Effect::StartMusic { .. }
                            | Effect::PrepareEarlyMusic { .. }
                            | Effect::MusicClassUnavailable { .. }
                            | Effect::ControlPlayback { .. }
                            | Effect::Playback { .. }
                            | Effect::Dtmf { .. }
                            | Effect::Gate { .. }
                            | Effect::PeerDirection { .. }
                            | Effect::Answer { .. }
                            | Effect::Ring { .. }
                            | Effect::Start { .. }
                            | Effect::Release { .. }
                            | Effect::Bridge { .. }
                            | Effect::ReleaseBridge { .. }
                            | Effect::DialBridge { .. } => None,
                            Effect::ConnectExternalRtp { key, .. }
                            | Effect::ConnectWebSocket { key, .. }
                            | Effect::ConnectAudioSocket { key, .. } => {
                                Some(Admission::ExternalMedia(key.clone()))
                            }
                        });
                        if let Err(error) = self.publish(&mut outcome) {
                            if reply.send(Err(error.clone())).is_err() {
                                tracing::debug!("control requester disconnected during overload");
                            }
                            return Err(error);
                        }
                        if let Some(admission) = admission {
                            let deadline =
                                now_ms.checked_add(5000).ok_or(ControlError::Capacity)?;
                            match admission {
                                Admission::Recording { owner, token } => {
                                    self.recording_admissions.insert(
                                        token,
                                        recording_admission::Pending::new(
                                            owner, reply, outcome, deadline,
                                        ),
                                    );
                                }
                                Admission::ExternalMedia(key) => {
                                    self.records.insert(
                                        calls::ADMISSION,
                                        key,
                                        external_admission::Pending::new(reply, deadline),
                                    )?;
                                }
                            }
                            return Ok(());
                        }
                        Ok(outcome)
                    }
                    Err(error) => Err(error),
                };
                if reply.send(result).is_err() {
                    tracing::debug!("control requester disconnected");
                }
            }
            Request::Subscribe {
                credentials,
                origin,
                application,
                events,
                lifetime,
                kind,
                reply,
            } => {
                if reply.is_closed() {
                    return Ok(());
                }
                if self.admissions_paused() {
                    if reply.send(Err(ControlError::Capacity)).is_err() {
                        tracing::debug!("Event subscriber disconnected during publication");
                    }
                    return Ok(());
                }
                let result = self
                    .authorize(&credentials, origin.as_deref(), true)
                    .and_then(|()| {
                        self.sessions
                            .retain(|_, session| !session.lifetime.is_closed());
                        if self.sessions.len() >= 64 {
                            return Err(ControlError::Capacity);
                        }
                        self.control.subscribe(&application)?;
                        self.subscription_generation = self
                            .subscription_generation
                            .checked_add(1)
                            .ok_or(ControlError::Capacity)?;
                        let token = self.subscription_generation;
                        self.sessions.insert(token, Session { lifetime, kind });
                        if let Some(previous) = self
                            .subscribers
                            .insert(application.clone(), Subscriber { events })
                        {
                            if previous
                                .events
                                .try_send(Event {
                                    timestamp: timestamp(),
                                    application,
                                    kind: EventKind::ApplicationReplaced {},
                                })
                                .is_err()
                            {
                                tracing::debug!("replaced application receiver disconnected");
                            }
                        }
                        Ok(token)
                    });
                if reply.send(result).is_err() {
                    tracing::debug!("event subscriber disconnected");
                }
            }
        }
        Ok(())
    }

    fn admit_incoming(
        &mut self,
        call: IncomingCall,
        now_ms: u64,
    ) -> Result<(CallKey, Outcome), ControlError> {
        let dialplan = matches!(&call.target, crate::IncomingTarget::Dialplan);
        if dialplan
            && self
                .control
                .published_routes()?
                .position(&call.context, &call.extension, 1)
                .is_none()
        {
            return Err(ControlError::NotFound("Extension not found"));
        }
        let id = call.id.clone();
        let timestamp = timestamp();
        let mut outcome = self.control.incoming(call, &timestamp)?;
        let key = self.control.key(&id)?;
        if dialplan {
            match self.control.start_bound_dialplan(&key, now_ms, &timestamp) {
                Ok(executed) => outcome.append(executed),
                Err(error) => {
                    outcome.append(self.control.apply(Action::Hangup(id), &timestamp, now_ms)?);
                    self.publish(&mut outcome)?;
                    return Err(error);
                }
            }
        }
        Ok((key, outcome))
    }

    pub(crate) fn wake(&self) -> Result<Wake, ControlError> {
        let mut wake = self.control.expiry_wake();
        if self.effects_flowing() && !self.admissions_paused() {
            wake = wake.earliest(self.control.dialplan_wake());
        }
        wake = wake.earliest(self.reload_wake()?);
        for pending in self.recording_admissions.values() {
            wake = wake.earliest(Wake::At(pending.deadline()));
        }
        for (key, pending) in self.records.iter(calls::ADMISSION) {
            wake = wake.earliest(if self.control.has_channel(key) {
                Wake::At(pending.deadline())
            } else {
                crate::overdue::DUE
            });
        }
        for (key, pending) in self.records.iter(calls::ANSWER) {
            wake = wake.earliest(if self.answer_waits(key) {
                pending.wake()
            } else {
                crate::overdue::DUE
            });
        }
        Ok(wake)
    }

    pub(crate) fn settle(&mut self, now: u64) -> Result<(), ControlError> {
        self.tick_reload()?;
        self.sessions
            .retain(|_, session| !session.lifetime.is_closed());
        let mut outcome = self.control.expire(now, &timestamp())?;
        self.publish(&mut outcome)?;
        if self.effects_flowing() && !self.admissions_paused() {
            let mut outcome = self.control.tick_dialplans(now, &timestamp())?;
            self.publish(&mut outcome)?;
        }
        let disconnected = self
            .subscribers
            .iter()
            .filter(|(_, subscriber)| subscriber.events.is_closed())
            .map(|(application, _)| application.clone())
            .collect::<Vec<_>>();
        for application in disconnected {
            self.subscribers.remove(&application);
            let mut outcome = self.control.unsubscribe(&application, &timestamp())?;
            self.publish(&mut outcome)?;
        }
        self.expire_recording_admissions(now)?;
        self.finish_answers(now);
        self.expire_external_admissions(now)?;
        self.expire_media_connections(now)?;
        Ok(())
    }

    async fn serve(&mut self) -> Result<(), ControlError> {
        let Outlet::Channel(effect_sender) = &self.effects else {
            return Err(ControlError::Conflict(
                "A composed control task runs inside signaling",
            ));
        };
        let effect_sender = effect_sender.clone();
        let clock = self.clock;
        let mut overdue = crate::overdue::Overdue::every(OVERDUE_RETRY_MS);
        let mut before = self.wake()?;
        loop {
            // Every input ends with its decisions settled and its effects handed over, as in the composed task.
            let now = clock.now_ms()?;
            self.settle(now)?;
            // A stop ends the hand-over, and the cleanup hands over what remains.
            while !self.pending_effects.is_empty() && !*self.shutdown.borrow() {
                tokio::select! {
                    permit = effect_sender.reserve() => match permit {
                        Ok(permit) => {
                            if let Some(effect) = self.pending_effects.pop_front() {
                                permit.send(effect);
                            }
                        }
                        Err(_) if *self.shutdown.borrow() => return Ok(()),
                        Err(_) => {
                            return Err(ControlError::Adapter(
                                "Signaling effect receiver closed".into(),
                            ))
                        }
                    },
                    changed = self.shutdown.changed() => {
                        if changed.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
            if *self.shutdown.borrow() {
                return Ok(());
            }
            overdue.settled(before, self.wake()?, now);
            let timer = overdue.timer(self.wake()?);
            before = self.wake()?;
            tokio::select! {
                input = self.next() => {
                    if self.handle(input, clock.now_ms()?)? == Flow::Stop { break; }
                }
                () = clock.reached(timer) => self.activity.control.deadline(timer, clock.now_ms()?),
            }
        }
        Ok(())
    }

    async fn drain_effects(&mut self, deadline: tokio::time::Instant) -> Result<(), ControlError> {
        let Outlet::Channel(sender) = &self.effects else {
            // The composed task ends every remaining call in its own cleanup.
            if !self.pending_effects.is_empty() {
                tracing::debug!(
                    pending = self.pending_effects.len(),
                    "pending effects end with the composed task"
                );
            }
            self.pending_effects.clear();
            return Ok(());
        };
        let sender = sender.clone();
        while !self.pending_effects.is_empty() {
            match tokio::time::timeout_at(deadline, sender.reserve()).await {
                Ok(Ok(permit)) => {
                    if let Some(effect) = self.pending_effects.pop_front() {
                        permit.send(effect);
                    }
                }
                Ok(Err(_)) if *self.shutdown.borrow() => {
                    tracing::debug!(
                        pending = self.pending_effects.len(),
                        "pending effects cancelled with the signaling scope"
                    );
                    self.pending_effects.clear();
                }
                Ok(Err(error)) => return Err(ControlError::Adapter(error.to_string())),
                Err(error) => return Err(ControlError::Adapter(error.to_string())),
            }
        }
        Ok(())
    }

    pub(crate) async fn close(&mut self) -> Result<(), ControlError> {
        self.records.retain(calls::CONNECTION, |_, _| false);
        self.records.retain(calls::LIFETIME, |_, _| false);
        self.close_recording_admissions();
        self.close_external_admissions();
        self.records.retain(calls::ANSWER, |_, _| false);
        let result = self
            .control
            .shutdown(&timestamp())
            .and_then(|mut outcome| self.publish(&mut outcome));
        self.subscribers.clear();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut cleanup = self.close_reload(deadline).await;
        if let Err(error) = self.drain_effects(deadline).await {
            cleanup = match cleanup {
                Ok(()) => Err(error),
                Err(primary) => Err(ControlError::Adapter(format!(
                    "{primary}; effect cleanup failed: {error}"
                ))),
            };
        }
        for (_, mut session) in std::mem::take(&mut self.sessions) {
            if session.kind == SubscriptionKind::Socket {
                let result = match tokio::time::timeout_at(deadline, session.lifetime.recv()).await
                {
                    Ok(None) => Ok(()),
                    Ok(Some(())) => Err(ControlError::Adapter(
                        "Unexpected connection lifetime signal".into(),
                    )),
                    Err(error) => Err(ControlError::Adapter(error.to_string())),
                };
                if let Err(error) = result {
                    tracing::warn!(%error, "event connection cleanup failed");
                    if cleanup.is_ok() {
                        cleanup = Err(error);
                    }
                }
            }
        }
        if let Err(error) = self.close_stored_queries(deadline).await {
            if cleanup.is_ok() {
                cleanup = Err(error);
            }
        }
        while !self.history_jobs.is_empty() {
            let failure = match tokio::time::timeout_at(deadline, self.history_jobs.join_next())
                .await
            {
                Ok(Some(Ok(Ok(())))) | Ok(None) => None,
                Ok(Some(Ok(Err(error)))) => Some(ControlError::Adapter(error.to_string())),
                Ok(Some(Err(error))) => Some(ControlError::Adapter(error.to_string())),
                Err(error) => {
                    self.history_jobs.abort_all();
                    while let Some(result) = self.history_jobs.join_next().await {
                        match result {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => {
                                tracing::error!(%error, "history failed before cancellation")
                            }
                            Err(error) => {
                                tracing::debug!(%error, "history task cancelled during shutdown")
                            }
                        }
                    }
                    Some(ControlError::Adapter(error.to_string()))
                }
            };
            if let Some(error) = failure {
                tracing::error!(%error, "history shutdown failed");
                if cleanup.is_ok() {
                    cleanup = Err(error);
                }
            }
        }
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(ControlError::Adapter(format!(
                "{primary}; event connection cleanup failed: {cleanup}"
            ))),
        }
    }

    pub async fn run(mut self) -> Result<(), ControlError> {
        let result = self.serve().await;
        if let Err(error) = &result {
            self.retain_reload_failure(error);
            self.stop.send_replace(true);
        }
        let cleanup = self.close().await;
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(ControlError::Adapter(format!(
                "{primary}; control cleanup failed: {cleanup}"
            ))),
        }
    }
}
