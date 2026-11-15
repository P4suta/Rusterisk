use super::failure;
use crate::ControlError;
use hickory_resolver::{
    config::{ResolverConfig, ResolverOpts},
    net::runtime::TokioRuntimeProvider,
    proto::rr::{rdata::SRV, Name, RData},
    TokioResolver,
};
use rusterisk_config::Protocol;
use rusterisk_sip::{StaticTarget, TargetHost};
use std::{
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

mod cache;
pub(super) use cache::ResolutionCache;
#[cfg(test)]
mod tests;

const MAX_CANDIDATES: usize = 8;
const QUERY_TIMEOUT: Duration = Duration::from_millis(500);
const RESOLUTION_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(super) struct DestinationResolver {
    inner: TokioResolver,
}

#[derive(Clone)]
pub(super) struct ResolvedTarget {
    target: StaticTarget,
    identity: String,
    peers: [Option<SocketAddr>; MAX_CANDIDATES],
    expires: Option<Instant>,
}

impl ResolvedTarget {
    pub(super) fn literal(target: &StaticTarget) -> Result<Option<Self>, ControlError> {
        let TargetHost::Address(address) = target.host() else {
            return Ok(None);
        };
        let default_port = match target.protocol() {
            Protocol::Udp | Protocol::Tcp => 5060,
            Protocol::Tls => 5061,
            Protocol::Ws | Protocol::Wss => return Err(ControlError::Unsupported),
        };
        let mut resolved = Self {
            target: target.clone(),
            identity: address.to_string(),
            peers: [None; MAX_CANDIDATES],
            expires: None,
        };
        resolved.insert(*address, target.port().unwrap_or(default_port))?;
        Ok(Some(resolved))
    }

    pub(super) fn protocol(&self) -> Protocol {
        self.target.protocol()
    }
    pub(super) fn identity(&self) -> &str {
        &self.identity
    }

    pub(super) fn peers(&self) -> impl Iterator<Item = SocketAddr> + '_ {
        self.peers.iter().flatten().copied()
    }

    pub(super) fn cache_valid(&self, now: Instant) -> bool {
        self.expires.is_some_and(|deadline| now < deadline)
    }

    fn insert(&mut self, address: IpAddr, port: u16) -> Result<(), ControlError> {
        if address.is_unspecified() || address.is_multicast() || port == 0 {
            return Err(ControlError::Invalid("Invalid resolved signaling address"));
        }
        let peer = SocketAddr::new(address, port);
        if self.peers().any(|current| current == peer) {
            return Ok(());
        }
        *self
            .peers
            .iter_mut()
            .find(|current| current.is_none())
            .ok_or(ControlError::Capacity)? = Some(peer);
        Ok(())
    }
}

impl DestinationResolver {
    pub(super) fn system() -> Result<Self, ControlError> {
        let (config, options) =
            hickory_resolver::system_conf::read_system_conf().map_err(failure)?;
        Self::with_config(config, options)
    }

    pub(super) fn with_config(
        config: ResolverConfig,
        mut options: ResolverOpts,
    ) -> Result<Self, ControlError> {
        if config.name_servers().len() > 8
            || config.search().len() > 8
            || config
                .name_servers()
                .iter()
                .any(|server| server.connections.len() > 2)
        {
            return Err(ControlError::Capacity);
        }
        options.timeout = QUERY_TIMEOUT;
        options.attempts = 1;
        options.cache_size = 0;
        options.max_active_requests = 16;
        options.num_concurrent_reqs = 1;
        options.edns_payload_len = 1232;
        let inner = TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
            .with_options(options)
            .build()
            .map_err(failure)?;
        Ok(Self { inner })
    }

    pub(super) async fn resolve(
        &self,
        target: &StaticTarget,
    ) -> Result<ResolvedTarget, ControlError> {
        tokio::time::timeout(RESOLUTION_TIMEOUT, self.resolve_inner(target))
            .await
            .map_err(failure)?
    }

    async fn resolve_inner(&self, target: &StaticTarget) -> Result<ResolvedTarget, ControlError> {
        if let Some(resolved) = ResolvedTarget::literal(target)? {
            return Ok(resolved);
        }
        let default_port = match target.protocol() {
            Protocol::Udp | Protocol::Tcp => 5060,
            Protocol::Tls => 5061,
            Protocol::Ws | Protocol::Wss => return Err(ControlError::Unsupported),
        };
        match target.host() {
            TargetHost::Address(address) => Err(ControlError::Adapter(format!(
                "Unresolved numeric signaling address: {address}"
            ))),
            TargetHost::Name(host) => {
                let name = Name::from_utf8(host).map_err(failure)?;
                if name.is_root() {
                    return Err(ControlError::Invalid("Empty signaling hostname"));
                }
                let identity = name.to_ascii().trim_end_matches('.').to_ascii_lowercase();
                let mut resolved = ResolvedTarget {
                    target: target.clone(),
                    identity,
                    peers: [None; MAX_CANDIDATES],
                    expires: None,
                };
                resolved.expires = match target.port() {
                    Some(port) => Some(self.addresses(&name, port, &mut resolved).await?),
                    None => {
                        self.services(&name, target.protocol(), default_port, &mut resolved)
                            .await?
                    }
                };
                Ok(resolved)
            }
        }
    }

    async fn services(
        &self,
        name: &Name,
        protocol: Protocol,
        default_port: u16,
        resolved: &mut ResolvedTarget,
    ) -> Result<Option<Instant>, ControlError> {
        let service = match protocol {
            Protocol::Udp => "_sip._udp",
            Protocol::Tcp => "_sip._tcp",
            Protocol::Tls => "_sips._tcp",
            Protocol::Ws | Protocol::Wss => return Err(ControlError::Unsupported),
        };
        let service =
            Name::from_ascii(format!("{service}.{}", name.to_ascii())).map_err(failure)?;
        let lookup = tokio::time::timeout(QUERY_TIMEOUT, self.inner.srv_lookup(service))
            .await
            .map_err(failure)?;
        let lookup = match lookup {
            Ok(lookup) => lookup,
            Err(error) if error.is_no_records_found() => {
                self.addresses(name, default_port, resolved).await?;
                return Ok(None);
            }
            Err(error) => return Err(failure(error)),
        };
        let mut expires = lookup.valid_until();
        let mut records = Vec::with_capacity(MAX_CANDIDATES);
        for record in lookup.answers() {
            if let RData::SRV(record) = &record.data {
                if records.len() == MAX_CANDIDATES {
                    return Err(ControlError::Capacity);
                }
                if record.target.is_root() {
                    return Err(ControlError::NotFound("Signaling service is unavailable"));
                }
                if record.port == 0 {
                    return Err(ControlError::Invalid("Invalid signaling service port"));
                }
                records.push(record.clone());
            }
        }
        if records.is_empty() {
            self.addresses(name, default_port, resolved).await?;
            return Ok(None);
        }
        let ordered = ordered_srv(records, |bound| rand::random_range(0..=bound))?;
        let mut last_failure = ControlError::NotFound("No signaling service addresses found");
        for record in ordered {
            match self.addresses(&record.target, record.port, resolved).await {
                Ok(deadline) => expires = expires.min(deadline),
                Err(error) => {
                    if matches!(error, ControlError::Capacity | ControlError::Invalid(_)) {
                        return Err(error);
                    }
                    last_failure = error;
                }
            }
        }
        if resolved.peers().next().is_some() {
            Ok(Some(expires))
        } else {
            Err(last_failure)
        }
    }

    async fn addresses(
        &self,
        name: &Name,
        port: u16,
        resolved: &mut ResolvedTarget,
    ) -> Result<Instant, ControlError> {
        let lookup = tokio::time::timeout(QUERY_TIMEOUT, self.inner.lookup_ip(name.clone()))
            .await
            .map_err(failure)?
            .map_err(failure)?;
        if lookup.iter().count() > MAX_CANDIDATES {
            return Err(ControlError::Capacity);
        }
        for address in lookup.iter() {
            resolved.insert(address, port)?;
        }
        if resolved.peers().next().is_none() {
            return Err(ControlError::NotFound("No signaling addresses found"));
        }
        Ok(lookup.valid_until())
    }
}

fn ordered_srv(
    mut records: Vec<SRV>,
    mut choose: impl FnMut(u32) -> u32,
) -> Result<Vec<SRV>, ControlError> {
    if records.len() > MAX_CANDIDATES {
        return Err(ControlError::Capacity);
    }
    records.sort_by_key(|record| (record.priority, record.weight != 0));
    let mut ordered = Vec::with_capacity(records.len());
    for _ in 0..records.len() {
        let priority = records.first().ok_or(ControlError::Capacity)?.priority;
        let sum: u32 = records
            .iter()
            .take_while(|record| record.priority == priority)
            .map(|record| u32::from(record.weight))
            .sum();
        let selection = choose(sum);
        if selection > sum {
            return Err(ControlError::Invalid("Invalid service weight selection"));
        }
        let mut accumulated = 0;
        let index = records
            .iter()
            .take_while(|record| record.priority == priority)
            .position(|record| {
                accumulated += u32::from(record.weight);
                accumulated >= selection
            })
            .ok_or(ControlError::Capacity)?;
        ordered.push(records.remove(index));
    }
    Ok(ordered)
}
