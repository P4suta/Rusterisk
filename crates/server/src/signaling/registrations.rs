use super::{
    cleanup, failure, media,
    transport::{
        ConnectionAdmission, ConnectionKey, ConnectionSetup, DeliveryRoute, Prepared, ProfileEpoch,
        SetupKey, SetupLifetime,
    },
    Received, Signaling,
};
use crate::ControlError;
use candidates::{CandidateCursor, Selection};
use peer_waits::{Admission, PeerWaits};
pub(super) use resolution::ConfigurationLease;
use resolution::{Completion, ConfigurationLifetime};
use rusterisk_config::{Auth, Config, Protocol, Registration, MAX_REGISTRATIONS};
use rusterisk_sip::{
    Datagram, RegistrationAuthentication, RegistrationGrant, RegistrationIdentity,
    RegistrationResponse, RegistrationRetirementReceipt, RegistrationTransaction,
    RegistrationTransactionEvent, StaticTarget, TargetHost,
};
use std::{
    net::SocketAddr,
    num::{NonZeroU32, NonZeroU64},
    time::Duration,
};
use tokio::task::JoinSet;
use tokio::time::Instant;
mod candidates;
mod peer_waits;
mod publication;
pub(super) use publication::PreparedRegistrations;
mod resolution;
mod retiring;
mod retry;
#[cfg(test)]
mod tests;

pub(super) struct Registrations {
    clients: Vec<Client>,
    cursor: usize,
    identity: RegistrationIdentity,
    lifetime: ConfigurationLifetime,
    jobs: JoinSet<Completion>,
    retiring: retiring::Retiring,
    pub(super) settings_epoch: Option<rusterisk_core::SettingsGeneration>,
    reservation: Option<publication::RegistrationReservation>,
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
    retries: u32,
    auth: Auth,
    candidates: Option<CandidateCursor>,
    waits: PeerWaits,
    continuation: Option<RegistrationResponse>,
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
    CandidateWait {
        at: u64,
    },
    Ready {
        at: u64,
        route: Route,
    },
    Alternate {
        at: u64,
        response: RegistrationResponse,
        peer: SocketAddr,
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
    Backoff {
        at: u64,
        response: Option<RegistrationResponse>,
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
            | Self::Registered { route, .. }
            | Self::Backoff { route, .. } => Some(*route),
            Self::Alternate { peer, .. } => Some(Route::Datagram(*peer)),
            Self::Waiting { .. }
            | Self::CandidateWait { .. }
            | Self::Resolving { .. }
            | Self::Connecting { .. }
            | Self::Stopped => None,
        }
    }
}

impl Client {
    fn failed_response(
        &mut self,
        response: RegistrationResponse,
        route: Route,
        now: u64,
    ) -> Result<State, ControlError> {
        let status = response.status();
        let instruction = response.retry_after().map_err(failure)?;
        if matches!(route, Route::Datagram(_)) {
            if let Some(interval) = instruction {
                self.waits.postpone(route.peer(), interval, now)?;
            } else {
                self.waits.release_unused();
            }
        }
        if status == 503 && matches!(route, Route::Datagram(_)) {
            if let Some(candidates) = &mut self.candidates {
                match candidates.select(&mut self.waits, route.peer().is_ipv4(), now)? {
                    Selection::Peer(peer) => {
                        return Ok(State::Alternate {
                            at: now,
                            response,
                            peer,
                        });
                    }
                    Selection::Wait { at } => {
                        self.continuation = Some(response);
                        return Ok(State::CandidateWait { at });
                    }
                    Selection::Exhausted => {}
                }
            }
        }
        self.retry(status, instruction, Some(response), route, now)
    }

    fn retry(
        &mut self,
        status: u16,
        instruction: Option<NonZeroU32>,
        response: Option<RegistrationResponse>,
        route: Route,
        now: u64,
    ) -> Result<State, ControlError> {
        self.waits.release_unused();
        match retry::decide(&self.settings, self.retries, status, instruction, now)? {
            retry::Decision::Stop => Ok(State::Stopped),
            retry::Decision::Wait { at, retries } => {
                self.retries = retries;
                Ok(State::Backoff {
                    at,
                    response,
                    route,
                })
            }
        }
    }

    fn choose_candidate(&mut self, ipv4: bool, now: u64) -> Result<State, ControlError> {
        let candidates = self
            .candidates
            .as_mut()
            .ok_or(ControlError::Invalid("Missing registration candidates"))?;
        Ok(match candidates.select(&mut self.waits, ipv4, now)? {
            Selection::Peer(peer) => State::Ready {
                at: now,
                route: Route::Datagram(peer),
            },
            Selection::Wait { at } => State::CandidateWait { at },
            Selection::Exhausted => State::Stopped,
        })
    }

    fn same_registrar(&self, other: &Self) -> bool {
        if self
            .state
            .peer()
            .zip(other.state.peer())
            .is_some_and(|(first, second)| first == second)
        {
            return true;
        }
        retiring::same_registrar(&self.target, &other.target)
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
        Self::new_at(config, 0)
    }

    pub(super) fn new_at(config: &Config, now: u64) -> Result<Self, ControlError> {
        let clients = Self::clients_at(config, now)?;
        Ok(Self {
            clients,
            cursor: 0,
            identity: RegistrationIdentity::new(random()?, 0, 0).map_err(failure)?,
            lifetime: ConfigurationLifetime::new(NonZeroU64::MIN),
            jobs: JoinSet::new(),
            retiring: retiring::Retiring::new(now),
            settings_epoch: None,
            reservation: None,
        })
    }

    fn clients_at(config: &Config, now: u64) -> Result<Vec<Client>, ControlError> {
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
            let delay = u64::try_from(delay)
                .map_err(failure)?
                .checked_add(1)
                .and_then(|delay| delay.checked_mul(1000))
                .ok_or(ControlError::Capacity)?;
            let at = now.checked_add(delay).ok_or(ControlError::Capacity)?;
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
                retries: 0,
                auth,
                candidates: None,
                waits: PeerWaits::default(),
                continuation: None,
                state,
            });
        }
        Ok(clients)
    }

    pub(super) const fn generation(&self) -> NonZeroU64 {
        self.lifetime.generation()
    }

    fn select(&mut self, now: u64) -> Option<usize> {
        if self.lifetime.lease().is_released() {
            return None;
        }
        for _ in 0..self.clients.len() {
            let index = self.cursor;
            self.cursor = (self.cursor + 1) % self.clients.len();
            let client = &self.clients[index];
            if !self.allows_transaction(index, client, now) {
                continue;
            }
            if self
                .retiring
                .blocked(&client.target, client.state.peer(), now)
            {
                continue;
            }
            let due = match &client.state {
                State::Waiting { at }
                | State::CandidateWait { at }
                | State::Registered { refresh_at: at, .. } => {
                    now >= *at
                        && !self.clients.iter().any(|other| {
                            client.same_registrar(other)
                                && matches!(
                                    other.state,
                                    State::Ready { .. }
                                        | State::Alternate { .. }
                                        | State::Trying { .. }
                                        | State::Resolving { .. }
                                        | State::Connecting { .. }
                                )
                        })
                }
                State::Ready { at, .. }
                | State::Alternate { at, .. }
                | State::Backoff { at, .. } => {
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
        if self.lifetime.lease().is_released() {
            return Ok(None);
        }
        self.retiring.poll(now)?;
        let current = self.clients.get(index).ok_or(ControlError::Capacity)?;
        if !self.allows_transaction(index, current, now) {
            return Ok(None);
        }
        let client = self.clients.get_mut(index).ok_or(ControlError::Capacity)?;
        if self
            .retiring
            .blocked(&client.target, client.state.peer(), now)
        {
            return Ok(None);
        }
        if matches!(
            &client.state,
            State::Ready { at, .. }
                | State::CandidateWait { at }
                | State::Alternate { at, .. }
                | State::Backoff { at, .. }
                | State::Registered { refresh_at: at, .. }
                if now < *at
        ) {
            return Ok(None);
        }
        if !matches!(client.state, State::Trying { .. }) {
            if let Some(Route::Datagram(peer)) = client.state.route() {
                if let Admission::Wait { at } = client.waits.reserve(peer, now)? {
                    match &mut client.state {
                        State::Ready { at: due, .. }
                        | State::Alternate { at: due, .. }
                        | State::Backoff { at: due, .. }
                        | State::Registered {
                            refresh_at: due, ..
                        } => *due = at,
                        State::Waiting { .. }
                        | State::CandidateWait { .. }
                        | State::Resolving { .. }
                        | State::Connecting { .. }
                        | State::Trying { .. }
                        | State::Stopped => {}
                    }
                    return Ok(None);
                }
            }
        }
        let transaction = match &client.state {
            State::Ready { route, .. }
            | State::Backoff {
                response: None,
                route,
                ..
            } => {
                let identity = self.identity.advance(random()?).map_err(failure)?;
                let mut settings = client.settings.clone();
                settings.expiration = client.requested_expiration;
                let transaction = match &client.continuation {
                    Some(response) => response
                        .retry_after_resolution(
                            std::slice::from_ref(&client.auth),
                            identity,
                            route.peer(),
                            now,
                        )
                        .map_err(failure)?,
                    None => {
                        RegistrationTransaction::new(&settings, local, route.peer(), identity, now)
                            .map_err(failure)?
                    }
                };
                Some((transaction, *route))
            }
            State::Registered {
                response, route, ..
            } => {
                let identity = self.identity.advance(random()?).map_err(failure)?;
                Some((
                    response
                        .refresh(
                            std::slice::from_ref(&client.auth),
                            identity,
                            NonZeroU32::new(client.requested_expiration).ok_or(
                                ControlError::Invalid("Refresh lifetime must be positive"),
                            )?,
                            now,
                        )
                        .map_err(failure)?,
                    *route,
                ))
            }
            State::Alternate { response, peer, .. } => {
                let identity = self.identity.advance(random()?).map_err(failure)?;
                Some((
                    response
                        .retry_alternate(std::slice::from_ref(&client.auth), identity, *peer, now)
                        .map_err(failure)?,
                    Route::Datagram(*peer),
                ))
            }
            State::Backoff {
                response: Some(response),
                route,
                ..
            } => {
                let identity = self.identity.advance(random()?).map_err(failure)?;
                Some((
                    response
                        .retry_after_failure(std::slice::from_ref(&client.auth), identity, now)
                        .map_err(failure)?,
                    *route,
                ))
            }
            State::Waiting { .. }
            | State::CandidateWait { .. }
            | State::Resolving { .. }
            | State::Connecting { .. }
            | State::Trying { .. }
            | State::Stopped => None,
        };
        if let Some((transaction, route)) = transaction {
            self.identity = transaction.identity();
            client.continuation = None;
            client.state = State::Trying { transaction, route };
        }
        let State::Trying { transaction, route } = &mut client.state else {
            return Ok(None);
        };
        let route = *route;
        match transaction.poll(now).map_err(failure)? {
            Some(RegistrationTransactionEvent::Transmit(packet)) => Ok(Some(packet)),
            Some(RegistrationTransactionEvent::TimedOut) => {
                client.state = client.retry(408, None, None, route, now)?;
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
                let identity = self.identity.advance(random()?).map_err(failure)?;
                match response
                    .authenticate(
                        std::slice::from_ref(&client.auth),
                        identity,
                        &format!("{:032x}", random()?),
                        1,
                        now,
                    )
                    .map_err(failure)?
                {
                    RegistrationAuthentication::Retry(transaction) => {
                        self.identity = identity;
                        State::Trying { transaction, route }
                    }
                    RegistrationAuthentication::Rejected(response) => {
                        client.failed_response(response, route, now)?
                    }
                }
            }
            200..=299 => {
                let grant = response.grant().map_err(failure)?;
                client.retries = 0;
                client.candidates = None;
                client.continuation = None;
                client.waits.release_unused();
                match grant {
                    RegistrationGrant::Active { refresh_at, .. } => State::Registered {
                        response,
                        refresh_at,
                        route,
                    },
                    RegistrationGrant::Removed => State::Stopped,
                }
            }
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
            _ => client.failed_response(response, route, now)?,
        };
        Ok(())
    }

    pub(super) fn cancel(&mut self) {
        self.lifetime.cancel();
        self.reservation = None;
        self.retiring.clear();
        for client in &mut self.clients {
            if let State::Trying { transaction, .. } = &mut client.state {
                transaction.cancel();
            }
            client.state = State::Stopped;
            client.candidates = None;
            client.continuation = None;
            client.waits = PeerWaits::default();
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
        let setup = self.network.reserve_setup(
            client.listener,
            self.registrations
                .settings_epoch
                .map(ProfileEpoch::Settings)
                .unwrap_or(ProfileEpoch::Configuration(self.registrations.generation())),
        )?;
        let key = ConfigurationKey {
            generation: self.registrations.lifetime.generation(),
            index,
        };
        let client = &mut self.registrations.clients[index];
        let previous = std::mem::replace(&mut client.state, State::Resolving { setup });
        client.continuation = match previous {
            State::Backoff { response, .. } => {
                if target.protocol() == Protocol::Udp {
                    response
                } else {
                    None
                }
            }
            State::Alternate { response, .. } => Some(response),
            State::Ready { .. } | State::Waiting { .. } | State::CandidateWait { .. } => {
                client.continuation.take()
            }
            State::Resolving { .. }
            | State::Connecting { .. }
            | State::Trying { .. }
            | State::Registered { .. }
            | State::Stopped => None,
        };
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
                client.candidates = Some(CandidateCursor::new(resolved));
                client.state = client.choose_candidate(local.is_ipv4(), now)?;
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
                        let route = self.network.capture_connection(connection)?;
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
            Ok(ConnectionAdmission::Continue) => {}
            Ok(ConnectionAdmission::Ready(connection)) => {
                let route = self.network.capture_connection(connection)?;
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
                if let State::Backoff { at, .. } = &client.state {
                    client.state = State::Waiting { at: *at };
                    continue;
                }
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
                | State::CandidateWait { .. }
                | State::Ready { .. }
                | State::Alternate { .. }
                | State::Trying { .. }
                | State::Registered { .. }
                | State::Backoff { .. }
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
        match self.registrations.retiring.receive_datagram(packet, now) {
            Ok(RegistrationRetirementReceipt::Unmatched) => {}
            Ok(
                RegistrationRetirementReceipt::Provisional(_)
                | RegistrationRetirementReceipt::Final(_),
            ) => return true,
            Err(error) => {
                tracing::debug!(%error, "retired registration response refused");
                return true;
            }
        }
        let Some(index) = self.registrations.clients.iter().position(|client| {
            client.listener == packet.listener
                && client.state.peer() == Some(packet.peer)
                && matches!(client.state, State::Trying { .. })
                && match client.state.route() {
                    Some(Route::Stream(route)) => route == packet.route(),
                    Some(Route::Datagram(_)) => true,
                    None => false,
                }
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
        self.registrations.retiring.poll(now)?;
        if !self.admitting() && self.registrations.reservation.is_none() {
            return Ok(());
        }
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
        if matches!(
            self.registrations.clients[index].state,
            State::CandidateWait { .. }
        ) {
            let listener = self.registrations.clients[index].listener;
            let ipv4 = self.network.local_addr(listener)?.is_ipv4();
            let client = &mut self.registrations.clients[index];
            client.state = client.choose_candidate(ipv4, now)?;
            if matches!(client.state, State::CandidateWait { .. } | State::Stopped) {
                return Ok(());
            }
        }
        let client = &self.registrations.clients[index];
        let listener = client.listener;
        let route = client
            .state
            .route()
            .ok_or(ControlError::Invalid("Registration has no signaling route"))?;
        if matches!(route, Route::Stream(route) if !self.network.current(route)) {
            self.registrations.clients[index].state = match &client.state {
                State::Backoff { at, .. } => State::Waiting { at: *at },
                State::Waiting { .. }
                | State::CandidateWait { .. }
                | State::Ready { .. }
                | State::Alternate { .. }
                | State::Resolving { .. }
                | State::Connecting { .. }
                | State::Trying { .. }
                | State::Registered { .. }
                | State::Stopped => State::Stopped,
            };
            return Ok(());
        }
        let mut local = self.network.local_addr(listener)?;
        if matches!(
            client.state,
            State::Ready { .. } | State::Backoff { response: None, .. }
        ) && local.ip().is_unspecified()
        {
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
