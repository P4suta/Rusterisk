use super::{
    cleanup, failure, media,
    transport::{ConnectionKey, ConnectionSetup, DeliveryRoute, Prepared, SetupKey, SetupLifetime},
    Received, Signaling,
};
use crate::ControlError;
pub(super) use resolution::ConfigurationLease;
use resolution::{Completion, ConfigurationLifetime};
use rusterisk_config::{Auth, Config, Protocol, Registration, MAX_REGISTRATIONS};
use rusterisk_sip::{
    Datagram, RegistrationGrant, RegistrationIdentity, RegistrationResponse,
    RegistrationTransaction, RegistrationTransactionEvent, StaticTarget, TargetHost,
};
use std::{
    net::SocketAddr,
    num::{NonZeroU32, NonZeroU64},
    time::{Duration, Instant},
};
use tokio::task::JoinSet;
mod resolution;
#[cfg(test)]
mod tests;

pub(super) struct Registrations {
    clients: Vec<Client>,
    cursor: usize,
    identity: RegistrationIdentity,
    lifetime: ConfigurationLifetime,
    jobs: JoinSet<Completion>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ConfigurationKey {
    generation: NonZeroU64,
    index: usize,
}

struct Client {
    listener: usize,
    target: StaticTarget,
    settings: Registration,
    requested_expiration: u32,
    auth: Auth,
    state: State,
}

#[derive(Clone, Copy)]
enum Route {
    Datagram(SocketAddr),
    Stream(DeliveryRoute),
}

impl Route {
    fn peer(self) -> SocketAddr {
        match self {
            Self::Datagram(peer) => peer,
            Self::Stream(route) => route.peer(),
        }
    }

    fn matches_connection(self, connection: ConnectionKey) -> bool {
        match self {
            Self::Datagram(_) => false,
            Self::Stream(route) => route.matches_connection(connection),
        }
    }
}

enum State {
    Waiting {
        at: u64,
    },
    Ready {
        at: u64,
        route: Route,
    },
    Resolving {
        setup: SetupKey,
    },
    Connecting {
        key: ConfigurationKey,
        setup: SetupKey,
        lease: ConfigurationLease,
    },
    Trying {
        transaction: RegistrationTransaction,
        route: Route,
    },
    Registered {
        response: RegistrationResponse,
        refresh_at: u64,
        route: Route,
    },
    Stopped,
}

impl State {
    fn peer(&self) -> Option<SocketAddr> {
        self.route().map(Route::peer)
    }

    fn route(&self) -> Option<Route> {
        match self {
            Self::Ready { route, .. }
            | Self::Trying { route, .. }
            | Self::Registered { route, .. } => Some(*route),
            Self::Waiting { .. }
            | Self::Resolving { .. }
            | Self::Connecting { .. }
            | Self::Stopped => None,
        }
    }
}

impl Client {
    fn same_registrar(&self, other: &Self) -> bool {
        if self
            .state
            .peer()
            .zip(other.state.peer())
            .is_some_and(|(first, second)| first == second)
        {
            return true;
        }
        let Ok(default_port) = default_port(self.target.protocol()) else {
            return false;
        };
        self.target.protocol() == other.target.protocol()
            && self.target.port().unwrap_or(default_port)
                == other.target.port().unwrap_or(default_port)
            && match (self.target.host(), other.target.host()) {
                (TargetHost::Address(first), TargetHost::Address(second)) => first == second,
                (TargetHost::Name(first), TargetHost::Name(second)) => first
                    .trim_end_matches('.')
                    .eq_ignore_ascii_case(second.trim_end_matches('.')),
                (TargetHost::Address(_), TargetHost::Name(_))
                | (TargetHost::Name(_), TargetHost::Address(_)) => false,
            }
    }
}

fn default_port(protocol: Protocol) -> Result<u16, ControlError> {
    match protocol {
        Protocol::Udp | Protocol::Tcp => Ok(5060),
        Protocol::Tls => Ok(5061),
        Protocol::Ws | Protocol::Wss => Err(ControlError::Unsupported),
    }
}

fn random() -> Result<u128, ControlError> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(failure)?;
    Ok(u128::from_be_bytes(bytes))
}

impl Registrations {
    pub(super) fn new(config: &Config) -> Result<Self, ControlError> {
        if config.registrations.len() > MAX_REGISTRATIONS {
            return Err(ControlError::Capacity);
        }
        let mut clients = Vec::with_capacity(config.registrations.len());
        for settings in &config.registrations {
            let listener = config
                .transports
                .iter()
                .position(|transport| transport.name == settings.transport)
                .ok_or(ControlError::NotFound("Registration transport not found"))?;
            let protocol = config.transports[listener].protocol;
            let default_port = default_port(protocol)?;
            if settings.server_uri.protocol() != protocol {
                return Err(ControlError::Unsupported);
            }
            let target =
                StaticTarget::from_uri(protocol, settings.server_uri.as_str()).map_err(failure)?;
            let delay = if settings.max_random_initial_delay == 0 {
                0
            } else {
                random()? % u128::from(settings.max_random_initial_delay)
            };
            let at = (u64::try_from(delay).map_err(failure)? + 1) * 1000;
            let state = match target.host() {
                TargetHost::Address(address) => {
                    let peer = SocketAddr::new(*address, target.port().unwrap_or(default_port));
                    if peer.is_ipv4() != config.transports[listener].bind.is_ipv4() {
                        return Err(ControlError::Invalid(
                            "Registration address family mismatch",
                        ));
                    }
                    if protocol == Protocol::Udp {
                        State::Ready {
                            at,
                            route: Route::Datagram(peer),
                        }
                    } else {
                        State::Waiting { at }
                    }
                }
                TargetHost::Name(_) => State::Waiting { at },
            };
            let auth = config
                .auth
                .iter()
                .find(|auth| auth.name == settings.outbound_auth)
                .ok_or(ControlError::NotFound("Registration credentials not found"))?
                .clone();
            clients.push(Client {
                listener,
                target,
                settings: settings.clone(),
                requested_expiration: settings.expiration,
                auth,
                state,
            });
        }
        let boot = if clients.is_empty() { 0 } else { random()? };
        Ok(Self {
            clients,
            cursor: 0,
            identity: RegistrationIdentity::new(boot, 0, 0).map_err(failure)?,
            lifetime: ConfigurationLifetime::new(NonZeroU64::MIN),
            jobs: JoinSet::new(),
        })
    }

    pub(super) const fn generation(&self) -> NonZeroU64 {
        self.lifetime.generation()
    }

    fn select(&mut self, now: u64) -> Option<usize> {
        for _ in 0..self.clients.len() {
            let index = self.cursor;
            self.cursor = (self.cursor + 1) % self.clients.len();
            let client = &self.clients[index];
            let due = match &client.state {
                State::Waiting { at } | State::Registered { refresh_at: at, .. } => {
                    now >= *at
                        && !self.clients.iter().any(|other| {
                            client.same_registrar(other)
                                && matches!(
                                    other.state,
                                    State::Ready { .. }
                                        | State::Trying { .. }
                                        | State::Resolving { .. }
                                        | State::Connecting { .. }
                                )
                        })
                }
                State::Ready { at, .. } => {
                    now >= *at
                        && !self.clients.iter().any(|other| {
                            client.same_registrar(other)
                                && matches!(
                                    other.state,
                                    State::Trying { .. }
                                        | State::Resolving { .. }
                                        | State::Connecting { .. }
                                )
                        })
                }
                State::Trying { transaction, .. } => transaction
                    .next_deadline()
                    .is_some_and(|deadline| now >= deadline),
                State::Resolving { .. } | State::Connecting { .. } | State::Stopped => false,
            };
            if due {
                return Some(index);
            }
        }
        None
    }

    fn poll(
        &mut self,
        index: usize,
        local: SocketAddr,
        now: u64,
    ) -> Result<Option<Datagram>, ControlError> {
        let client = self.clients.get_mut(index).ok_or(ControlError::Capacity)?;
        let transaction = match &client.state {
            State::Ready { route, .. } => {
                self.identity = self.identity.advance(random()?).map_err(failure)?;
                let mut settings = client.settings.clone();
                settings.expiration = client.requested_expiration;
                Some((
                    RegistrationTransaction::new(
                        &settings,
                        local,
                        route.peer(),
                        self.identity,
                        now,
                    )
                    .map_err(failure)?,
                    *route,
                ))
            }
            State::Registered {
                response, route, ..
            } => {
                self.identity = self.identity.advance(random()?).map_err(failure)?;
                Some((
                    response
                        .refresh(
                            std::slice::from_ref(&client.auth),
                            self.identity,
                            NonZeroU32::new(client.requested_expiration).ok_or(
                                ControlError::Invalid("Refresh lifetime must be positive"),
                            )?,
                            now,
                        )
                        .map_err(failure)?,
                    *route,
                ))
            }
            State::Waiting { .. }
            | State::Resolving { .. }
            | State::Connecting { .. }
            | State::Trying { .. }
            | State::Stopped => None,
        };
        if let Some((transaction, route)) = transaction {
            client.state = State::Trying { transaction, route };
        }
        let State::Trying { transaction, .. } = &mut client.state else {
            return Ok(None);
        };
        match transaction.poll(now).map_err(failure)? {
            Some(RegistrationTransactionEvent::Transmit(packet)) => Ok(Some(packet)),
            Some(RegistrationTransactionEvent::TimedOut) => {
                client.state = State::Stopped;
                Ok(None)
            }
            Some(
                RegistrationTransactionEvent::Provisional(_)
                | RegistrationTransactionEvent::Response(_),
            ) => Err(ControlError::Invalid("Unexpected registration poll event")),
            None => Ok(None),
        }
    }

    fn response(
        &mut self,
        index: usize,
        response: RegistrationResponse,
        now: u64,
    ) -> Result<(), ControlError> {
        let client = self.clients.get_mut(index).ok_or(ControlError::Capacity)?;
        let route = client
            .state
            .route()
            .ok_or(ControlError::Invalid("Registration response has no route"))?;
        client.state = match response.status() {
            401 | 407 => {
                self.identity = self.identity.advance(random()?).map_err(failure)?;
                match response
                    .authenticated_retry(
                        std::slice::from_ref(&client.auth),
                        self.identity,
                        &format!("{:032x}", random()?),
                        1,
                        now,
                    )
                    .map_err(failure)?
                {
                    Some(transaction) => State::Trying { transaction, route },
                    None => return Err(ControlError::Unsupported),
                }
            }
            200..=299 => match response.grant().map_err(failure)? {
                RegistrationGrant::Active { refresh_at, .. } => State::Registered {
                    response,
                    refresh_at,
                    route,
                },
                RegistrationGrant::Removed => State::Stopped,
            },
            423 => match response.minimum_expiration().map_err(failure)? {
                Some(expiration) => {
                    let identity = self.identity.advance(random()?).map_err(failure)?;
                    let transaction = response
                        .minimum_expiration_retry(std::slice::from_ref(&client.auth), identity, now)
                        .map_err(failure)?
                        .ok_or(ControlError::Invalid("Missing registration correction"))?;
                    self.identity = identity;
                    client.requested_expiration = expiration.get();
                    State::Trying { transaction, route }
                }
                None => State::Stopped,
            },
            _ => State::Stopped,
        };
        Ok(())
    }

    pub(super) fn cancel(&mut self) {
        self.lifetime.cancel();
        for client in &mut self.clients {
            if let State::Trying { transaction, .. } = &mut client.state {
                transaction.cancel();
            }
            client.state = State::Stopped;
        }
    }

    pub(super) fn setup_count(&self) -> usize {
        self.jobs.len()
    }

    pub(super) async fn completed(&mut self) -> Result<Option<Completion>, ControlError> {
        self.jobs.join_next().await.transpose().map_err(failure)
    }
}

impl Signaling {
    fn resolve_registration(&mut self, index: usize) -> Result<(), ControlError> {
        let lease = self.registrations.lifetime.lease();
        if lease.is_released() {
            return Err(ControlError::Conflict("Registration configuration ended"));
        }
        let client = self
            .registrations
            .clients
            .get(index)
            .ok_or(ControlError::Capacity)?;
        let target = client.target.clone();
        let cached = self.resolution_cache.get(&target, Instant::now());
        let setup = self
            .network
            .reserve_setup(client.listener, self.registrations.generation())?;
        let key = ConfigurationKey {
            generation: self.registrations.lifetime.generation(),
            index,
        };
        self.registrations.clients[index].state = State::Resolving { setup };
        self.registrations.jobs.spawn(resolution::resolve(
            key,
            setup,
            lease,
            target,
            cached,
            self.resolver.clone(),
        ));
        Ok(())
    }

    pub(super) fn registration_completed(
        &mut self,
        completed: Completion,
        now: u64,
    ) -> Result<(), ControlError> {
        if !self
            .registrations
            .lifetime
            .is_current(completed.key, &completed.lease)
        {
            self.network.finish_resolution(completed.setup);
            return Ok(());
        }
        let Some(client) = self.registrations.clients.get_mut(completed.key.index) else {
            self.network.finish_resolution(completed.setup);
            return Ok(());
        };
        if !matches!(client.state, State::Resolving { setup } if setup == completed.setup) {
            self.network.finish_resolution(completed.setup);
            return Ok(());
        }
        let resolved = match completed.result {
            Ok(resolved) => resolved,
            Err(error) => {
                self.network.finish_resolution(completed.setup);
                client.state = State::Stopped;
                tracing::debug!(registration = %client.settings.name, %error, "registration resolution failed");
                return Ok(());
            }
        };
        self.resolution_cache
            .insert(resolved.clone(), Instant::now())?;
        match resolved.protocol() {
            Protocol::Udp => {
                self.network.finish_resolution(completed.setup);
                let local = self.network.local_addr(client.listener)?;
                let Some(peer) = resolved
                    .peers()
                    .find(|peer| peer.is_ipv4() == local.is_ipv4())
                else {
                    client.state = State::Stopped;
                    return Ok(());
                };
                client.state = State::Ready {
                    at: now,
                    route: Route::Datagram(peer),
                };
            }
            Protocol::Tcp | Protocol::Tls => {
                client.state = State::Connecting {
                    key: completed.key,
                    setup: completed.setup,
                    lease: completed.lease.clone(),
                };
                match self.network.connect_prepared(
                    completed.setup,
                    resolved,
                    SetupLifetime::Configuration(completed.lease),
                ) {
                    Ok(ConnectionSetup::Reused(connection)) => {
                        let route = self.network.capture(connection.listener, connection.peer)?;
                        client.state = State::Ready {
                            at: now,
                            route: Route::Stream(route),
                        };
                    }
                    Ok(ConnectionSetup::Connecting) => {}
                    Err(error) => {
                        self.network.cancel_setup(completed.setup);
                        self.network.finish_resolution(completed.setup);
                        client.state = State::Stopped;
                        tracing::debug!(registration = %client.settings.name, %error, "registration connection failed");
                    }
                }
            }
            Protocol::Ws | Protocol::Wss => {
                self.network.finish_resolution(completed.setup);
                client.state = State::Stopped;
            }
        }
        Ok(())
    }

    pub(super) fn registration_owns_setup(&self, key: SetupKey) -> bool {
        self.registrations
            .clients
            .iter()
            .any(|client| matches!(client.state, State::Connecting { setup, .. } if setup == key))
    }

    pub(super) fn registration_prepared(
        &mut self,
        prepared: Prepared,
        now: u64,
    ) -> Result<(), ControlError> {
        let Some(client) = self.registrations.clients.iter_mut().find(|client| matches!(client.state, State::Connecting { setup, .. } if setup == prepared.key)) else {
            self.network.cancel_setup(prepared.key);
            return Ok(());
        };
        let State::Connecting { key, lease, .. } = &client.state else {
            return Err(ControlError::Invalid("Unexpected registration preparation"));
        };
        if !self.registrations.lifetime.is_current(*key, lease) {
            self.network.cancel_setup(prepared.key);
            client.state = State::Stopped;
            return Ok(());
        }
        let setup = prepared.key;
        match self.network.admit_prepared(prepared) {
            Ok(connection) => {
                let route = self.network.capture(connection.listener, connection.peer)?;
                client.state = State::Ready {
                    at: now,
                    route: Route::Stream(route),
                };
            }
            Err(error) => {
                self.network.cancel_setup(setup);
                client.state = State::Stopped;
                tracing::debug!(registration = %client.settings.name, %error, "registration admission failed");
            }
        }
        Ok(())
    }

    pub(super) fn registration_setup_finished(
        &mut self,
        key: SetupKey,
        result: Result<(), ControlError>,
    ) {
        if let Some(client) =
            self.registrations.clients.iter_mut().find(
                |client| matches!(client.state, State::Connecting { setup, .. } if setup == key),
            )
        {
            client.state = State::Stopped;
            if let Err(error) = result {
                tracing::debug!(registration = %client.settings.name, %error, "registration setup stopped");
            }
        }
    }

    pub(super) fn registration_closed(&mut self, key: ConnectionKey) {
        for client in &mut self.registrations.clients {
            if client
                .state
                .route()
                .is_some_and(|route| route.matches_connection(key))
            {
                if let State::Trying { transaction, .. } = &mut client.state {
                    transaction.cancel();
                }
                client.state = State::Stopped;
            }
        }
    }

    pub(super) async fn finish_registrations(&mut self) -> Result<(), ControlError> {
        let setups = self
            .registrations
            .clients
            .iter()
            .filter_map(|client| match client.state {
                State::Resolving { setup } | State::Connecting { setup, .. } => Some(setup),
                State::Waiting { .. }
                | State::Ready { .. }
                | State::Trying { .. }
                | State::Registered { .. }
                | State::Stopped => None,
            })
            .collect::<Vec<_>>();
        for setup in &setups {
            self.network.cancel_setup(*setup);
        }
        self.registrations.cancel();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut result = Ok(());
        while !self.registrations.jobs.is_empty() {
            match tokio::time::timeout_at(deadline, self.registrations.jobs.join_next()).await {
                Ok(Some(Ok(completed))) => self.network.finish_resolution(completed.setup),
                Ok(Some(Err(error))) => result = cleanup::combine(result, Err(failure(error))),
                Ok(None) => break,
                Err(error) => {
                    self.registrations.jobs.abort_all();
                    while let Some(completed) = self.registrations.jobs.join_next().await {
                        match completed {
                            Ok(completed) => self.network.finish_resolution(completed.setup),
                            Err(error) if error.is_cancelled() => {}
                            Err(error) => result = cleanup::combine(result, Err(failure(error))),
                        }
                    }
                    result = cleanup::combine(result, Err(failure(error)));
                    break;
                }
            }
        }
        for setup in setups {
            self.network.finish_resolution(setup);
        }
        result
    }

    pub(super) fn registration_received(&mut self, packet: &Received, now: u64) -> bool {
        let Some(index) = self.registrations.clients.iter().position(|client| {
            client.listener == packet.listener
                && client.state.peer() == Some(packet.peer)
                && matches!(client.state, State::Trying { .. })
        }) else {
            return false;
        };
        if matches!(self.registrations.clients[index].state.route(), Some(Route::Stream(route)) if !self.network.current(route))
        {
            self.registrations.clients[index].state = State::Stopped;
            return false;
        }
        let State::Trying { transaction, .. } = &mut self.registrations.clients[index].state else {
            return false;
        };
        let event = match transaction.receive(&packet.bytes, packet.peer, now) {
            Ok(event) => event,
            Err(error) => {
                tracing::debug!(%error, "registration response refused");
                return true;
            }
        };
        let response = match event {
            Some(RegistrationTransactionEvent::Response(response)) => response,
            Some(RegistrationTransactionEvent::Provisional(_)) => return true,
            Some(
                RegistrationTransactionEvent::Transmit(_) | RegistrationTransactionEvent::TimedOut,
            )
            | None => return false,
        };
        if let Err(error) = self.registrations.response(index, response, now) {
            let client = &mut self.registrations.clients[index];
            client.state = State::Stopped;
            tracing::debug!(registration = %client.settings.name, %error, "outbound registration stopped");
        }
        true
    }

    pub(super) async fn registration_tick(&mut self, now: u64) -> Result<(), ControlError> {
        let Some(index) = self.registrations.select(now) else {
            return Ok(());
        };
        if matches!(
            self.registrations.clients[index].state,
            State::Waiting { .. }
        ) {
            return match self.resolve_registration(index) {
                Ok(()) | Err(ControlError::Capacity) => Ok(()),
                Err(error) => {
                    self.registrations.clients[index].state = State::Stopped;
                    tracing::debug!(%error, "registration resolution stopped");
                    Ok(())
                }
            };
        }
        let client = &self.registrations.clients[index];
        let listener = client.listener;
        let route = client
            .state
            .route()
            .ok_or(ControlError::Invalid("Registration has no signaling route"))?;
        if matches!(route, Route::Stream(route) if !self.network.current(route)) {
            self.registrations.clients[index].state = State::Stopped;
            return Ok(());
        }
        let mut local = self.network.local_addr(listener)?;
        if matches!(client.state, State::Ready { .. }) && local.ip().is_unspecified() {
            local.set_ip(media::local_ip(route.peer())?);
        }
        let result = async {
            if let Some(packet) = self.registrations.poll(index, local, now)? {
                match route {
                    Route::Datagram(_) => self.network.send(listener, packet).await?,
                    Route::Stream(route) => {
                        if !self.network.send_on(route, packet).await? {
                            return Err(ControlError::Conflict(
                                "Registration signaling flow closed",
                            ));
                        }
                    }
                }
            }
            Ok::<(), ControlError>(())
        }
        .await;
        if let Err(error) = result {
            let client = &mut self.registrations.clients[index];
            client.state = State::Stopped;
            tracing::debug!(registration = %client.settings.name, %error, "outbound registration stopped");
        }
        Ok(())
    }
}
