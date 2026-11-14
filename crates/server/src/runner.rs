use crate::{Action, CallKey, Control, ControlError, Effect, IncomingCall, Outcome, Signal};
use rusterisk_api::{Event, EventKind};
use rusterisk_config::Config;
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, oneshot, watch};

const MAX_PENDING_EFFECTS: usize = 64 * 2 + 1;

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
            },
            Runner {
                control: Control::new(capacity),
                config,
                requests: request_receiver,
                signals: signal_receiver,
                effects,
                pending_effects: VecDeque::with_capacity(MAX_PENDING_EFFECTS),
                shutdown: shutdown_receiver,
                subscribers: BTreeMap::new(),
                sessions: BTreeMap::new(),
                subscription_generation: 0,
            },
            effect_receiver,
        )
    }

    pub fn shutdown(&self) {
        self.shutdown.send_replace(true);
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
        let (reply, receiver) = oneshot::channel();
        self.requests
            .try_send(Request::Apply {
                credentials,
                origin,
                action,
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
        for effect in outcome.effects.drain(..) {
            self.emit_effect(effect)?;
        }
        let mut disconnected = Vec::new();
        for event in outcome.events.drain(..) {
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
            for effect in cleanup.effects.drain(..) {
                self.emit_effect(effect)?;
            }
        }
        Ok(())
    }

    fn request(&mut self, request: Request, now_ms: u64) -> Result<(), ControlError> {
        match request {
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
                let id = call.id.clone();
                let result = match self.control.incoming(call, &timestamp()) {
                    Ok(mut outcome) => {
                        if let Err(error) = self.publish(&mut outcome) {
                            if reply.send(Err(error.clone())).is_err() {
                                tracing::debug!(
                                    "incoming admission requester disconnected during overload"
                                );
                            }
                            return Err(error);
                        }
                        self.control.key(&id)
                    }
                    Err(error) => Err(error),
                };
                if let Err(Ok(key)) = reply.send(result) {
                    let mut cleanup =
                        self.control
                            .apply(Action::Hangup(key.id), &timestamp(), now_ms)?;
                    self.publish(&mut cleanup)?;
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
                reply,
            } => {
                if reply.is_closed() {
                    return Ok(());
                }
                let result = self
                    .authorize(&credentials, origin.as_deref(), action.read_only())
                    .and_then(|()| {
                        if (!self.pending_effects.is_empty() && !action.read_only())
                            || self.effects.capacity() < action.effect_budget()
                        {
                            return Err(ControlError::Capacity);
                        }
                        self.control.apply(action, &timestamp(), now_ms)
                    });
                let result = match result {
                    Ok(mut outcome) => {
                        if let Err(error) = self.publish(&mut outcome) {
                            if reply.send(Err(error.clone())).is_err() {
                                tracing::debug!("control requester disconnected during overload");
                            }
                            return Err(error);
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

    async fn serve(&mut self) -> Result<(), ControlError> {
        let effect_sender = self.effects.clone();
        let started = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                permit = effect_sender.reserve(), if !self.pending_effects.is_empty() => {
                    match permit {
                        Ok(permit) => if let Some(effect) = self.pending_effects.pop_front() { permit.send(effect); },
                        Err(_) if *self.shutdown.borrow() => break,
                        Err(error) => return Err(ControlError::Adapter(error.to_string())),
                    }
                }
                changed = self.shutdown.changed() => { if changed.is_err() || *self.shutdown.borrow() { break; } }
                signal = self.signals.recv(), if self.pending_effects.is_empty() => { if let Some(signal) = signal { let mut outcome = self.control.signal(signal, &timestamp())?; self.publish(&mut outcome)?; } else { break; } }
                request = self.requests.recv() => {
                    if let Some(request) = request { self.request(request, u64::try_from(started.elapsed().as_millis()).map_err(|_| ControlError::Capacity)?)?; } else { break; }
                }
                _instant = tick.tick() => {
                    self.sessions.retain(|_, session| !session.lifetime.is_closed());
                    let mut outcome = self.control.expire(u64::try_from(started.elapsed().as_millis()).map_err(|_| ControlError::Capacity)?, &timestamp())?; self.publish(&mut outcome)?;
                    let disconnected: Vec<String> = self.subscribers.iter().filter(|(_, subscriber)| subscriber.events.is_closed()).map(|(application, _)| application.clone()).collect();
                    for application in disconnected { self.subscribers.remove(&application); let mut outcome = self.control.unsubscribe(&application, &timestamp())?; self.publish(&mut outcome)?; }
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
