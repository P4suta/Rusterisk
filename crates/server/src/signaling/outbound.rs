use super::{
    failure,
    resolver::{DestinationResolver, ResolvedTarget},
    transport::{Prepared, RegisteredRoute, SetupKey},
    Signaling,
};
use crate::{runner::CallLease, CallKey, ControlError, Controller, Signal};
use rusterisk_config::Protocol;
use rusterisk_sip::Destination;
use std::time::{Duration, Instant};
use tokio::sync::watch;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
enum Route {
    Registered(RegisteredRoute),
    Static,
}

enum Phase {
    Resolving,
    Connecting(CallLease),
}

pub(super) struct Pending {
    endpoint: String,
    caller: String,
    timeout: Option<u32>,
    listener: usize,
    setup: SetupKey,
    route: Route,
    phase: Phase,
    cancel: watch::Sender<bool>,
}

struct Resolution {
    lease: CallLease,
    target: Option<ResolvedTarget>,
}

pub(super) struct Completion {
    key: CallKey,
    setup: SetupKey,
    result: Result<Resolution, ControlError>,
}

impl Signaling {
    pub(super) fn queue_native(
        &mut self,
        key: CallKey,
        endpoint: String,
        caller: String,
        timeout: Option<u32>,
        now: u64,
    ) -> Result<(), ControlError> {
        if self.native_pending.len() >= 16
            || self.native_jobs.len() >= 16
            || self.calls.len()
                + self.external_media.len()
                + self.stream_admissions.len()
                + self.websocket_admissions.len()
                + self.native_pending.len()
                >= 64
            || self.native_pending.contains_key(&key)
            || self.calls.contains_key(&key)
        {
            return Err(ControlError::Capacity);
        }
        let name = endpoint
            .strip_prefix("PJSIP/")
            .ok_or(ControlError::Unsupported)?;
        let settings = self
            .config
            .endpoints
            .iter()
            .find(|endpoint| endpoint.name == name)
            .ok_or(ControlError::NotFound("Endpoint not found"))?;
        let listener = self
            .listeners
            .iter()
            .position(|listener| listener.name == settings.transport)
            .ok_or(ControlError::NotFound("Transport not found"))?;
        let destination = self.listeners[listener]
            .engine
            .destination(name, now)
            .map_err(failure)?;
        let cached = match &destination {
            Destination::Registered(_) => None,
            Destination::Static(target) => self.resolution_cache.get(target, Instant::now()),
        };
        let (route, setup) = match destination {
            Destination::Registered(peer) => {
                let route = self.network.registered_route(listener, peer)?;
                let setup = self.network.reserve_registered_setup(listener)?;
                (Route::Registered(route), setup)
            }
            Destination::Static(_) => {
                let setup = self.network.reserve_setup(listener)?;
                (Route::Static, setup)
            }
        };
        let (cancel, mut cancelled) = watch::channel(false);
        self.native_pending.insert(
            key.clone(),
            Pending {
                endpoint,
                caller,
                timeout,
                listener,
                setup,
                route,
                phase: Phase::Resolving,
                cancel,
            },
        );
        let resolver = self.resolver.clone();
        let controller = self.controller.clone();
        self.native_jobs.spawn(async move {
            let result = tokio::select! {
                _changed = cancelled.changed() => Err(ControlError::Conflict("Signaling resolution cancelled")),
                result = tokio::time::timeout(Duration::from_secs(3), resolve(controller, key.clone(), destination, cached, resolver)) => result.map_err(failure).and_then(|result| result),
            };
            Completion { key, setup, result }
        });
        Ok(())
    }

    pub(super) async fn native_completed(
        &mut self,
        completed: Completion,
        now: u64,
    ) -> Result<(), ControlError> {
        let Some(pending) = self.native_pending.get_mut(&completed.key) else {
            self.network.finish_resolution(completed.setup);
            return Ok(());
        };
        if !matches!(pending.phase, Phase::Resolving) {
            return Err(ControlError::Invalid(
                "Unexpected signaling resolution completion",
            ));
        }
        let resolved = match completed.result {
            Ok(resolved) if !resolved.lease.is_released() => resolved,
            Ok(_) => {
                self.cancel_native(&completed.key);
                self.network.finish_resolution(completed.setup);
                return Ok(());
            }
            Err(error) => {
                self.cancel_native(&completed.key);
                self.network.finish_resolution(completed.setup);
                return self.native_failed(completed.key, error).await;
            }
        };
        match (pending.route, resolved.target) {
            (Route::Registered(route), None) => {
                self.network.finish_resolution(pending.setup);
                let name = pending
                    .endpoint
                    .strip_prefix("PJSIP/")
                    .ok_or(ControlError::Unsupported)?;
                if !self.network.route_is_current(route)
                    || self.listeners[pending.listener]
                        .engine
                        .registered_peer(name, now)
                        != Some(route.peer())
                {
                    self.cancel_native(&completed.key);
                    return self
                        .native_failed(
                            completed.key,
                            ControlError::Conflict("Registered signaling flow changed"),
                        )
                        .await;
                }
                let pending = self
                    .native_pending
                    .remove(&completed.key)
                    .ok_or(ControlError::Capacity)?;
                let result = self
                    .start_resolved(
                        completed.key.clone(),
                        pending.endpoint,
                        pending.caller,
                        pending.timeout,
                        None,
                        now,
                    )
                    .await;
                if let Err(error) = result {
                    self.native_failed(completed.key, error).await?;
                }
            }
            (Route::Static, Some(target)) => {
                let setup = pending.setup;
                self.resolution_cache
                    .insert(target.clone(), Instant::now())?;
                match target.protocol() {
                    Protocol::Udp => {
                        let local = self.network.local_addr(pending.listener)?;
                        let peer = target
                            .peers()
                            .find(|peer| peer.is_ipv4() == local.is_ipv4());
                        self.network.finish_resolution(setup);
                        let pending = self
                            .native_pending
                            .remove(&completed.key)
                            .ok_or(ControlError::Capacity)?;
                        let result = match peer {
                            Some(peer) => {
                                self.start_resolved(
                                    completed.key.clone(),
                                    pending.endpoint,
                                    pending.caller,
                                    pending.timeout,
                                    Some(peer),
                                    now,
                                )
                                .await
                            }
                            None => Err(ControlError::NotFound("No usable signaling address")),
                        };
                        if let Err(error) = result {
                            self.native_failed(completed.key, error).await?;
                        }
                    }
                    Protocol::Tcp | Protocol::Tls => {
                        pending.phase = Phase::Connecting(resolved.lease.clone());
                        if let Err(error) =
                            self.network.connect_prepared(setup, target, resolved.lease)
                        {
                            self.cancel_native(&completed.key);
                            self.network.finish_resolution(setup);
                            self.native_failed(completed.key, error).await?;
                        }
                    }
                    Protocol::Ws | Protocol::Wss => return Err(ControlError::Unsupported),
                }
            }
            (Route::Registered(_), Some(_)) | (Route::Static, None) => {
                return Err(ControlError::Invalid("Signaling destination changed"))
            }
        }
        Ok(())
    }

    pub(super) async fn native_prepared(
        &mut self,
        prepared: Prepared,
        now: u64,
    ) -> Result<(), ControlError> {
        let key = self.native_pending.iter().find_map(|(key, pending)| {
            (matches!(pending.route, Route::Static) && pending.setup == prepared.key)
                .then(|| key.clone())
        });
        let Some(key) = key else {
            self.network.cancel_setup(prepared.key);
            return Ok(());
        };
        let pending = self
            .native_pending
            .get(&key)
            .ok_or(ControlError::Capacity)?;
        match &pending.phase {
            Phase::Connecting(lease) if !lease.is_released() => {}
            Phase::Connecting(_) | Phase::Resolving => {
                self.cancel_native(&key);
                return Ok(());
            }
        }
        let connection = match self.network.admit_prepared(prepared) {
            Ok(connection) => connection,
            Err(error) => {
                self.cancel_native(&key);
                return self.native_failed(key, error).await;
            }
        };
        let pending = self
            .native_pending
            .remove(&key)
            .ok_or(ControlError::Capacity)?;
        let result = self
            .start_resolved(
                key.clone(),
                pending.endpoint,
                pending.caller,
                pending.timeout,
                Some(connection.peer),
                now,
            )
            .await;
        if let Err(error) = result {
            self.native_failed(key, error).await?;
        }
        Ok(())
    }

    pub(super) async fn native_setup_finished(
        &mut self,
        setup: SetupKey,
        result: Result<(), ControlError>,
    ) -> Result<(), ControlError> {
        let key = self.native_pending.iter().find_map(|(key, pending)| {
            (matches!(pending.route, Route::Static) && pending.setup == setup).then(|| key.clone())
        });
        if let Some(key) = key {
            self.cancel_native(&key);
            let error = result.err().unwrap_or(ControlError::Conflict(
                "Signaling setup ended before admission",
            ));
            self.native_failed(key, error).await?;
        }
        Ok(())
    }

    pub(super) fn cancel_native(&mut self, key: &CallKey) {
        if let Some(pending) = self.native_pending.remove(key) {
            pending.cancel.send_replace(true);
            self.network.cancel_setup(pending.setup);
        }
    }

    async fn native_failed(&self, key: CallKey, error: ControlError) -> Result<(), ControlError> {
        tracing::debug!(channel = %key.id.as_str(), generation = key.generation, %error, "outgoing signaling setup failed");
        self.notify(Signal::Terminated { key, cause: 21 }).await
    }

    pub(super) async fn finish_native(&mut self) -> Result<(), ControlError> {
        let pending = std::mem::take(&mut self.native_pending);
        let mut setups = Vec::with_capacity(pending.len());
        for pending in pending.into_values() {
            pending.cancel.send_replace(true);
            self.network.cancel_setup(pending.setup);
            setups.push(pending.setup);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut outcome = Ok(());
        while !self.native_jobs.is_empty() {
            match tokio::time::timeout_at(deadline, self.native_jobs.join_next()).await {
                Ok(Some(Ok(completed))) => {
                    self.network.finish_resolution(completed.setup);
                }
                Ok(Some(Err(error))) => {
                    outcome = super::cleanup::combine(outcome, Err(failure(error)));
                }
                Ok(None) => break,
                Err(error) => {
                    self.native_jobs.abort_all();
                    while let Some(result) = self.native_jobs.join_next().await {
                        match result {
                            Ok(completed) => self.network.finish_resolution(completed.setup),
                            Err(error) if error.is_cancelled() => {}
                            Err(error) => {
                                outcome = super::cleanup::combine(outcome, Err(failure(error)));
                            }
                        }
                    }
                    outcome = super::cleanup::combine(outcome, Err(failure(error)));
                    break;
                }
            }
        }
        for setup in setups {
            self.network.finish_resolution(setup);
        }
        outcome
    }
}

async fn resolve(
    controller: Controller,
    key: CallKey,
    destination: Destination,
    cached: Option<ResolvedTarget>,
    resolver: Result<DestinationResolver, ControlError>,
) -> Result<Resolution, ControlError> {
    let lease = controller.call_lifetime(key).await?;
    let lookup = async {
        match destination {
            Destination::Registered(_) => Ok(None),
            Destination::Static(target) => match cached.or(ResolvedTarget::literal(&target)?) {
                Some(resolved) => Ok(Some(resolved)),
                None => resolver?.resolve(&target).await.map(Some),
            },
        }
    };
    let target = tokio::select! {
        _released = lease.released() => return Err(ControlError::Conflict("Call ended during signaling resolution")),
        result = lookup => result?,
    };
    Ok(Resolution { lease, target })
}
