use crate::storage::{StoragePort, WorkerError};
use crate::{Action, CallKey, Control, ControlError, Effect, IncomingCall, Outcome, Signal};
use rusterisk_api::{Event, EventKind};
use rusterisk_config::Config;
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;
mod call_lifetime;
mod external_admission;
mod history;
mod media_connections;
mod recording_admission;
mod stored_recordings;
pub(crate) use call_lifetime::CallLease;
use history::HistoryRecorder;
pub use media_connections::{ClaimedMediaConnection, MediaConnectionLease};
pub(crate) use stored_recordings::Operation as StoredRecordingOperation;
pub(crate) use stored_recordings::Response as StoredRecordingResponse;

struct StorageServices {
    port: StoragePort,
    history: HistoryRecorder,
}

const MAX_PENDING_EFFECTS: usize = 64 * 2 + 1;

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
    Unsubscribe {
        application: String,
        token: u64,
    },
}

struct Subscriber {
    token: u64,
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
    requests: mpsc::Sender<Request>,
    signals: mpsc::Sender<Signal>,
    shutdown: watch::Sender<bool>,
    info: crate::server_info::ServerInfo,
}

pub struct Runner {
    control: Control,
    config: Config,
    requests: mpsc::Receiver<Request>,
    signals: mpsc::Receiver<Signal>,
    effects: mpsc::Sender<Effect>,
    pending_effects: VecDeque<Effect>,
    shutdown: watch::Receiver<bool>,
    subscribers: BTreeMap<String, Subscriber>,
    sessions: BTreeMap<u64, Session>,
    subscription_generation: u64,
    storage: Option<StorageServices>,
    history_jobs: JoinSet<Result<(), WorkerError>>,
    stored_jobs: JoinSet<()>,
    recording_admissions: BTreeMap<rusterisk_core::MediaTaskToken, recording_admission::Pending>,
    external_admissions: BTreeMap<CallKey, external_admission::Pending>,
    media_leases: BTreeMap<CallKey, mpsc::Receiver<()>>,
    call_lifetimes: BTreeMap<CallKey, mpsc::Receiver<()>>,
}

pub struct Subscription {
    pub application: String,
    pub token: u64,
    pub events: mpsc::Receiver<Event>,
    controller: Controller,
    _lifetime: mpsc::Sender<()>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if self
            .controller
            .requests
            .try_send(Request::Unsubscribe {
                application: self.application.clone(),
                token: self.token,
            })
            .is_err()
        {
            tracing::debug!(application = %self.application, "subscription cleanup deferred to the control task");
        }
    }
}

async fn receive<T>(
    receiver: oneshot::Receiver<Result<T, ControlError>>,
) -> Result<T, ControlError> {
    tokio::time::timeout(Duration::from_secs(5), receiver)
        .await
        .map_err(|_| ControlError::Capacity)?
        .map_err(|_| ControlError::Adapter("Control task stopped".into()))?
}

impl Controller {
    pub async fn incoming(&self, call: IncomingCall) -> Result<CallKey, ControlError> {
        let (reply, receiver) = oneshot::channel();
        self.requests
            .try_send(Request::Incoming { call, reply })
            .map_err(|_| ControlError::Capacity)?;
        receive(receiver).await
    }

    pub fn new(config: Config, capacity: usize) -> (Self, Runner, mpsc::Receiver<Effect>) {
        let (requests, request_receiver) = mpsc::channel(128);
        let (signals, signal_receiver) = mpsc::channel(128);
        let (effects, effect_receiver) = mpsc::channel(128);
        let (shutdown, shutdown_receiver) = watch::channel(false);
        (
            Self {
                requests,
                signals,
                shutdown,
                info: crate::server_info::ServerInfo::new(capacity),
            },
            Runner {
                control: Control::with_configuration(capacity, &config),
                config,
                requests: request_receiver,
                signals: signal_receiver,
                effects,
                pending_effects: VecDeque::with_capacity(MAX_PENDING_EFFECTS),
                shutdown: shutdown_receiver,
                subscribers: BTreeMap::new(),
                sessions: BTreeMap::new(),
                subscription_generation: 0,
                storage: None,
                history_jobs: JoinSet::new(),
                stored_jobs: JoinSet::new(),
                recording_admissions: BTreeMap::new(),
                external_admissions: BTreeMap::new(),
                media_leases: BTreeMap::new(),
                call_lifetimes: BTreeMap::new(),
            },
            effect_receiver,
        )
    }

    pub fn shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    pub(crate) fn server_info(&self, only: Option<&str>) -> rusterisk_api::AsteriskInfo {
        self.info.selected(only)
    }

    pub fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    pub fn is_stopping(&self) -> bool {
        *self.shutdown.borrow()
    }

    pub async fn signal(&self, signal: Signal) -> Result<(), ControlError> {
        self.signals
            .try_send(signal)
            .map_err(|_| ControlError::Capacity)
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
            .map_err(|_| ControlError::Capacity)?;
        receive(receiver).await
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
            .map_err(|_| ControlError::Capacity)?;
        receive(receiver).await
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
            .map_err(|_| ControlError::Capacity)?;
        let token = receive(response).await?;
        Ok(Subscription {
            application,
            token,
            events: receiver,
            controller: self.clone(),
            _lifetime: lifetime,
        })
    }
}

fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl Runner {
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

    fn emit_effect(&mut self, effect: Effect) -> Result<(), ControlError> {
        if let Effect::Release { key } = &effect {
            self.call_lifetimes.remove(key);
        }
        let effect = if self.pending_effects.is_empty() {
            match self.effects.try_send(effect) {
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
        } else {
            effect
        };
        if self.pending_effects.len() >= MAX_PENDING_EFFECTS {
            return Err(ControlError::Capacity);
        }
        self.pending_effects.push_back(effect);
        Ok(())
    }

    fn publish(&mut self, outcome: &mut Outcome) -> Result<(), ControlError> {
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
                if subscriber.events.try_send(event).is_err()
                    && !disconnected.contains(&application)
                {
                    disconnected.push(application);
                }
            }
        }
        for application in disconnected {
            self.subscribers.remove(&application);
            let mut cleanup = self.control.unsubscribe(&application, &timestamp())?;
            self.publish_history(&mut cleanup)?;
            for effect in cleanup.effects.drain(..) {
                self.emit_effect(effect)?;
            }
        }
        Ok(())
    }

    fn request(&mut self, request: Request, now_ms: u64) -> Result<(), ControlError> {
        match request {
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
                if !self.pending_effects.is_empty() {
                    if reply.send(Err(ControlError::Capacity)).is_err() {
                        tracing::debug!("incoming requester disconnected during pending cleanup");
                    }
                    return Ok(());
                }
                let result = match self.admit_incoming(call, now_ms) {
                    Ok((key, mut outcome)) => {
                        if let Err(error) = self.publish(&mut outcome) {
                            if reply.send(Err(error.clone())).is_err() {
                                tracing::debug!(
                                    "incoming admission requester disconnected during overload"
                                );
                            }
                            return Err(error);
                        }
                        Ok(key)
                    }
                    Err(error) => Err(error),
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
                        if (!self.pending_effects.is_empty() && !action.read_only())
                            || self.effects.capacity() < self.control.effect_budget(&action)
                            || (recording
                                && (self.recording_admissions.len() >= 64
                                    || now_ms.checked_add(5000).is_none()))
                            || (external
                                && (self.external_admissions.len() >= 64
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
                            Effect::ControlRecording { .. }
                            | Effect::ReserveWebSocket { .. }
                            | Effect::EndRecording { .. }
                            | Effect::PlayMedia { .. }
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
                                    self.external_admissions.insert(
                                        key,
                                        external_admission::Pending::new(reply, deadline),
                                    );
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
                            .insert(application.clone(), Subscriber { token, events })
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
            Request::Unsubscribe { application, token } => {
                self.sessions.remove(&token);
                if self
                    .subscribers
                    .get(&application)
                    .is_some_and(|subscriber| subscriber.token == token)
                {
                    self.subscribers.remove(&application);
                    let mut outcome = self.control.unsubscribe(&application, &timestamp())?;
                    self.publish(&mut outcome)?;
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
            && !self.config.dialplan.steps.iter().any(|step| {
                step.context == call.context
                    && step.extension == call.extension
                    && step.priority == 1
            })
        {
            return Err(ControlError::NotFound("Extension not found"));
        }
        let id = call.id.clone();
        let timestamp = timestamp();
        let mut outcome = self.control.incoming(call, &timestamp)?;
        let key = self.control.key(&id)?;
        if dialplan {
            match self.control.start_dialplan(
                &key,
                self.config.dialplan.clone(),
                now_ms,
                &timestamp,
            ) {
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

    fn tick(&mut self, now: u64) -> Result<(), ControlError> {
        self.sessions
            .retain(|_, session| !session.lifetime.is_closed());
        let mut outcome = self.control.expire(now, &timestamp())?;
        self.publish(&mut outcome)?;
        if self.pending_effects.is_empty() {
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
        self.expire_external_admissions(now)?;
        self.expire_media_connections(now)?;
        Ok(())
    }

    async fn serve(&mut self) -> Result<(), ControlError> {
        let effect_sender = self.effects.clone();
        let started = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                result = self.stored_jobs.join_next(), if !self.stored_jobs.is_empty() => {
                    result.ok_or(ControlError::Capacity)?.map_err(|error| ControlError::Adapter(error.to_string()))?;
                }
                result = self.history_jobs.join_next(), if !self.history_jobs.is_empty() => {
                    result.ok_or(ControlError::Capacity)?
                        .map_err(|error| ControlError::Adapter(error.to_string()))?
                        .map_err(|error| ControlError::Adapter(error.to_string()))?;
                }
                permit = effect_sender.reserve(), if !self.pending_effects.is_empty() => {
                    match permit {
                        Ok(permit) => if let Some(effect) = self.pending_effects.pop_front() { permit.send(effect); },
                        Err(_) if *self.shutdown.borrow() => break,
                        Err(error) => return Err(ControlError::Adapter(error.to_string())),
                    }
                }
                changed = self.shutdown.changed() => { if changed.is_err() || *self.shutdown.borrow() { break; } }
                signal = self.signals.recv(), if self.pending_effects.is_empty() => { if let Some(signal) = signal { self.apply_signal(signal, u64::try_from(started.elapsed().as_millis()).map_err(|_| ControlError::Capacity)?)?; } else { break; } }
                request = self.requests.recv() => {
                    if let Some(request) = request { self.request(request, u64::try_from(started.elapsed().as_millis()).map_err(|_| ControlError::Capacity)?)?; } else { break; }
                }
                _instant = tick.tick() => {
                    self.tick(u64::try_from(started.elapsed().as_millis()).map_err(|_| ControlError::Capacity)?)?;
                }
            }
        }
        Ok(())
    }

    async fn drain_effects(&mut self, deadline: tokio::time::Instant) -> Result<(), ControlError> {
        while !self.pending_effects.is_empty() {
            match tokio::time::timeout_at(deadline, self.effects.reserve()).await {
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

    async fn close(&mut self) -> Result<(), ControlError> {
        self.media_leases.clear();
        self.call_lifetimes.clear();
        self.close_recording_admissions();
        self.close_external_admissions();
        let result = self
            .control
            .shutdown(&timestamp())
            .and_then(|mut outcome| self.publish(&mut outcome));
        self.subscribers.clear();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut cleanup = self.drain_effects(deadline).await;
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
