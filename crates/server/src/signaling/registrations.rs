use super::{failure, media, Signaling};
use crate::ControlError;
use rusterisk_config::{Config, Protocol, Registration, MAX_REGISTRATIONS};
use rusterisk_sip::{
    Datagram, RegistrationIdentity, RegistrationTransaction, RegistrationTransactionEvent,
    StaticTarget, TargetHost,
};
use std::net::SocketAddr;
#[cfg(test)]
mod tests;

pub(super) struct Registrations {
    clients: Vec<Client>,
    cursor: usize,
    identity: RegistrationIdentity,
}

struct Client {
    listener: usize,
    peer: SocketAddr,
    settings: Registration,
    state: State,
}

enum State {
    Waiting { at: u64 },
    Trying(RegistrationTransaction),
    Stopped,
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
            let TargetHost::Address(address) = target.host() else {
                return Err(ControlError::Unsupported);
            };
            let peer = SocketAddr::new(*address, target.port().unwrap_or(5060));
            if peer.is_ipv4() != config.transports[listener].bind.is_ipv4() {
                return Err(ControlError::Invalid(
                    "Registration address family mismatch",
                ));
            }
            let delay = if settings.max_random_initial_delay == 0 {
                0
            } else {
                random()? % u128::from(settings.max_random_initial_delay)
            };
            let at = (u64::try_from(delay).map_err(failure)? + 1) * 1000;
            clients.push(Client {
                listener,
                peer,
                settings: settings.clone(),
                state: State::Waiting { at },
            });
        }
        let boot = if clients.is_empty() { 0 } else { random()? };
        Ok(Self {
            clients,
            cursor: 0,
            identity: RegistrationIdentity::new(boot, 0, 0).map_err(failure)?,
        })
    }

    fn select(&mut self, now: u64) -> Option<usize> {
        for _ in 0..self.clients.len() {
            let index = self.cursor;
            self.cursor = (self.cursor + 1) % self.clients.len();
            let client = &self.clients[index];
            let due = match &client.state {
                State::Waiting { at } => {
                    now >= *at
                        && !self.clients.iter().any(|other| {
                            other.peer == client.peer && matches!(other.state, State::Trying(_))
                        })
                }
                State::Trying(transaction) => transaction
                    .next_deadline()
                    .is_some_and(|deadline| now >= deadline),
                State::Stopped => false,
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
        if let State::Waiting { .. } = client.state {
            self.identity = self.identity.advance(random()?).map_err(failure)?;
            client.state = State::Trying(
                RegistrationTransaction::new(
                    &client.settings,
                    local,
                    client.peer,
                    self.identity,
                    now,
                )
                .map_err(failure)?,
            );
        }
        let State::Trying(transaction) = &mut client.state else {
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

    pub(super) fn cancel(&mut self) {
        for client in &mut self.clients {
            if let State::Trying(transaction) = &mut client.state {
                transaction.cancel();
            }
            client.state = State::Stopped;
        }
    }
}

impl Signaling {
    pub(super) async fn registration_tick(&mut self, now: u64) -> Result<(), ControlError> {
        let Some(index) = self.registrations.select(now) else {
            return Ok(());
        };
        let client = &self.registrations.clients[index];
        let listener = client.listener;
        let mut local = self.network.local_addr(listener)?;
        if local.ip().is_unspecified() && matches!(client.state, State::Waiting { .. }) {
            local.set_ip(media::local_ip(client.peer)?);
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
