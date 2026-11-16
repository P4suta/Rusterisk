use super::{cleanup, failure, media, transport::SetupKey, Received, Signaling};
use crate::ControlError;
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
    auth: Auth,
    state: State,
}

enum State {
    Waiting {
        at: u64,
    },
    Ready {
        at: u64,
        peer: SocketAddr,
    },
    Resolving {
        setup: SetupKey,
    },
    Trying {
        transaction: RegistrationTransaction,
        peer: SocketAddr,
    },
    Registered {
        response: RegistrationResponse,
        refresh_at: u64,
    },
    Stopped,
}

impl State {
    fn peer(&self) -> Option<SocketAddr> {
        match self {
            Self::Ready { peer, .. } | Self::Trying { peer, .. } => Some(*peer),
            Self::Registered { response, .. } => Some(response.peer()),
            Self::Waiting { .. } | Self::Resolving { .. } | Self::Stopped => None,
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
        self.target.protocol() == other.target.protocol()
            && self.target.port().unwrap_or(5060) == other.target.port().unwrap_or(5060)
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
            if config.transports[listener].protocol != Protocol::Udp
                || settings.server_uri.protocol() != Protocol::Udp
            {
                return Err(ControlError::Unsupported);
            }
            let target = StaticTarget::from_uri(Protocol::Udp, settings.server_uri.as_str())
                .map_err(failure)?;
            let delay = if settings.max_random_initial_delay == 0 {
                0
            } else {
                random()? % u128::from(settings.max_random_initial_delay)
            };
            let at = (u64::try_from(delay).map_err(failure)? + 1) * 1000;
            let state = match target.host() {
                TargetHost::Address(address) => {
                    let peer = SocketAddr::new(*address, target.port().unwrap_or(5060));
                    if peer.is_ipv4() != config.transports[listener].bind.is_ipv4() {
                        return Err(ControlError::Invalid(
                            "Registration address family mismatch",
                        ));
                    }
                    State::Ready { at, peer }
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

    fn select(&mut self, now: u64) -> Option<usize> {
        for _ in 0..self.clients.len() {
            let index = self.cursor;
            self.cursor = (self.cursor + 1) % self.clients.len();
            let client = &self.clients[index];
            let due = match &client.state {
                State::Waiting { at }
                | State::Ready { at, .. }
                | State::Registered { refresh_at: at, .. } => {
                    now >= *at
                        && !self.clients.iter().any(|other| {
                            client.same_registrar(other)
                                && matches!(
                                    other.state,
                                    State::Trying { .. } | State::Resolving { .. }
                                )
                        })
                }
                State::Trying { transaction, .. } => transaction
                    .next_deadline()
                    .is_some_and(|deadline| now >= deadline),
                State::Resolving { .. } | State::Stopped => false,
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
            State::Ready { peer, .. } => {
                self.identity = self.identity.advance(random()?).map_err(failure)?;
                Some((
                    RegistrationTransaction::new(
                        &client.settings,
                        local,
                        *peer,
                        self.identity,
                        now,
                    )
                    .map_err(failure)?,
                    *peer,
                ))
            }
            State::Registered { response, .. } => {
                self.identity = self.identity.advance(random()?).map_err(failure)?;
                Some((
                    response
                        .refresh(
                            std::slice::from_ref(&client.auth),
                            self.identity,
                            NonZeroU32::new(client.settings.expiration).ok_or(
                                ControlError::Invalid("Refresh lifetime must be positive"),
                            )?,
                            now,
                        )
                        .map_err(failure)?,
                    response.peer(),
                ))
            }
            State::Waiting { .. }
            | State::Resolving { .. }
            | State::Trying { .. }
            | State::Stopped => None,
        };
        if let Some((transaction, peer)) = transaction {
            client.state = State::Trying { transaction, peer };
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
        let peer = client
            .state
            .peer()
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
                    Some(transaction) => State::Trying { transaction, peer },
                    None => return Err(ControlError::Unsupported),
                }
            }
            200..=299 => match response.grant().map_err(failure)? {
                RegistrationGrant::Active { refresh_at, .. } => State::Registered {
                    response,
                    refresh_at,
                },
                RegistrationGrant::Removed => State::Stopped,
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
        let setup = self.network.reserve_setup(client.listener)?;
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
        self.network.finish_resolution(completed.setup);
        if !self
            .registrations
            .lifetime
            .is_current(completed.key, &completed.lease)
        {
            return Ok(());
        }
        let Some(client) = self.registrations.clients.get_mut(completed.key.index) else {
            return Ok(());
        };
        if !matches!(client.state, State::Resolving { setup } if setup == completed.setup) {
            return Ok(());
        }
        let resolved = match completed.result {
            Ok(resolved) => resolved,
            Err(error) => {
                client.state = State::Stopped;
                tracing::debug!(registration = %client.settings.name, %error, "registration resolution failed");
                return Ok(());
            }
        };
        let local = self.network.local_addr(client.listener)?;
        let Some(peer) = resolved
            .peers()
            .find(|peer| peer.is_ipv4() == local.is_ipv4())
        else {
            client.state = State::Stopped;
            return Ok(());
        };
        self.resolution_cache.insert(resolved, Instant::now())?;
        client.state = State::Ready { at: now, peer };
        Ok(())
    }

    pub(super) async fn finish_registrations(&mut self) -> Result<(), ControlError> {
        let setups = self
            .registrations
            .clients
            .iter()
            .filter_map(|client| match client.state {
                State::Resolving { setup } => Some(setup),
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
        let mut local = self.network.local_addr(listener)?;
        if let State::Ready { peer, .. } = client.state {
            if local.ip().is_unspecified() {
                local.set_ip(media::local_ip(peer)?);
            }
        }
        let result = async {
            if let Some(packet) = self.registrations.poll(index, local, now)? {
                self.network.send(listener, packet).await?;
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
