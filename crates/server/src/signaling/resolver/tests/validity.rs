use super::{failure, DestinationResolver, ResolutionCache};
use crate::{
    signaling::resolver::{ResolutionValidity, ResolvedTarget},
    ControlError,
};
use hickory_resolver::{
    config::{LookupIpStrategy, NameServerConfig, ResolveHosts, ResolverConfig, ResolverOpts},
    proto::{
        op::{Message, OpCode},
        rr::{
            rdata::{A, AAAA},
            Name, RData, Record, RecordType,
        },
    },
};
use rusterisk_config::Protocol;
use rusterisk_sip::StaticTarget;
use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};
use tokio::net::UdpSocket;
use tokio::time::Instant;

#[derive(Clone, Copy)]
enum Family {
    Ipv4,
    Ipv6,
}

impl Family {
    fn strategy(self) -> LookupIpStrategy {
        match self {
            Self::Ipv4 => LookupIpStrategy::Ipv4Only,
            Self::Ipv6 => LookupIpStrategy::Ipv6Only,
        }
    }

    fn kind(self) -> RecordType {
        match self {
            Self::Ipv4 => RecordType::A,
            Self::Ipv6 => RecordType::AAAA,
        }
    }

    fn record(self, name: Name, ttl: u32) -> Record {
        Record::from_rdata(
            name,
            ttl,
            match self {
                Self::Ipv4 => RData::A(A(Ipv4Addr::LOCALHOST)),
                Self::Ipv6 => RData::AAAA(AAAA(Ipv6Addr::LOCALHOST)),
            },
        )
    }

    fn peer(self, port: u16) -> SocketAddr {
        match self {
            Self::Ipv4 => SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            Self::Ipv6 => SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
        }
    }
}

#[derive(Clone, Copy)]
enum Lookup {
    ExplicitPort,
    ServiceFallback,
}

impl Lookup {
    fn target(self) -> Result<StaticTarget, ControlError> {
        StaticTarget::from_uri(
            Protocol::Udp,
            match self {
                Self::ExplicitPort => "sip:account@peer.test:5070",
                Self::ServiceFallback => "sip:account@peer.test",
            },
        )
        .map_err(failure)
    }

    fn port(self) -> u16 {
        match self {
            Self::ExplicitPort => 5070,
            Self::ServiceFallback => 5060,
        }
    }
}

fn require(valid: bool, reason: &'static str) -> Result<(), ControlError> {
    if valid {
        Ok(())
    } else {
        Err(ControlError::Invalid(reason))
    }
}

async fn answer(
    socket: &UdpSocket,
    name: &str,
    kind: RecordType,
    records: Vec<Record>,
) -> Result<(), ControlError> {
    let mut bytes = [0; 1233];
    let (size, peer) = tokio::time::timeout(Duration::from_secs(1), socket.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    require(size <= 1232, "Oversized DNS lifetime query")?;
    let request = Message::from_vec(&bytes[..size]).map_err(failure)?;
    require(
        request.queries.len() == 1,
        "Expected one DNS lifetime question",
    )?;
    let query = request.queries.first().ok_or(ControlError::Capacity)?;
    require(
        query.name().to_ascii() == name && query.query_type() == kind,
        "Unexpected DNS lifetime question",
    )?;
    let mut response = Message::response(request.metadata.id, OpCode::Query);
    response.metadata.authoritative = true;
    response.metadata.recursion_available = true;
    response.metadata.recursion_desired = request.metadata.recursion_desired;
    response.queries.push(query.clone());
    response.answers = records;
    response.edns = request.edns;
    let bytes = response.to_vec().map_err(failure)?;
    require(bytes.len() <= 1232, "Oversized DNS lifetime answer")?;
    require(
        socket.send_to(&bytes, peer).await.map_err(failure)? == bytes.len(),
        "Incomplete DNS lifetime answer",
    )
}

async fn script(
    socket: &UdpSocket,
    lookup: Lookup,
    family: Family,
    ttl: u32,
) -> Result<(), ControlError> {
    match lookup {
        Lookup::ExplicitPort => {}
        Lookup::ServiceFallback => {
            answer(socket, "_sip._udp.peer.test.", RecordType::SRV, Vec::new()).await?
        }
    }
    let name = Name::from_ascii("peer.test.").map_err(failure)?;
    answer(
        socket,
        "peer.test.",
        family.kind(),
        std::iter::once(family.record(name, ttl)).collect(),
    )
    .await
}

struct Observation {
    result: ResolvedTarget,
    before: Instant,
    after: Instant,
}

async fn resolve(lookup: Lookup, family: Family, ttl: u32) -> Result<Observation, ControlError> {
    let socket = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let address = socket.local_addr().map_err(failure)?;
    let mut server = NameServerConfig::udp(address.ip());
    server
        .connections
        .first_mut()
        .ok_or(ControlError::Capacity)?
        .port = address.port();
    let mut options = ResolverOpts::default();
    options.ip_strategy = family.strategy();
    options.use_hosts_file = ResolveHosts::Never;
    options.positive_min_ttl = Some(Duration::from_secs(3600));
    let resolver = DestinationResolver::with_config(
        ResolverConfig::from_name_servers(std::iter::once(server).collect()),
        options,
    )?;
    let target = lookup.target()?;
    let before = Instant::now();
    let (result, answered) = tokio::join!(
        resolver.resolve(&target),
        script(&socket, lookup, family, ttl)
    );
    let after = Instant::now();
    drop(socket);
    let rebound = UdpSocket::bind(address)
        .await
        .map(|_socket| ())
        .map_err(failure);
    let cleanup = crate::signaling::cleanup::combine(answered, rebound);
    let result = match (result, cleanup) {
        (Ok(result), Ok(())) => result,
        (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
        (Err(primary), Err(cleanup)) => {
            return Err(failure(format!(
                "{primary}; DNS lifetime cleanup failed: {cleanup}"
            )))
        }
    };
    require(
        result.peers().eq([family.peer(lookup.port())]),
        "DNS lifetime lookup changed its candidate",
    )?;
    Ok(Observation {
        result,
        before,
        after,
    })
}

fn deadline(result: &ResolvedTarget) -> Option<Instant> {
    match result.validity {
        ResolutionValidity::Literal => None,
        ResolutionValidity::Dns { until, .. } => Some(until),
    }
}

fn original_deadline(observation: &Observation, ttl: u32) -> Result<Instant, ControlError> {
    let deadline = deadline(&observation.result).ok_or(ControlError::Invalid(
        "Uncacheable DNS fallback lost the address record deadline",
    ))?;
    let ttl = Duration::from_secs(u64::from(ttl));
    require(
        deadline >= observation.before + ttl && deadline <= observation.after + ttl,
        "DNS lifetime metadata extended or shortened the wire TTL",
    )?;
    Ok(deadline)
}

#[tokio::test]
async fn uncacheable_fallbacks_preserve_address_deadlines_without_entering_the_cache(
) -> Result<(), ControlError> {
    for family in [Family::Ipv4, Family::Ipv6] {
        let observation = resolve(Lookup::ServiceFallback, family, 60).await?;
        original_deadline(&observation, 60)?;
        let target = observation.result.target.clone();
        let mut cache = ResolutionCache::default();
        require(
            !observation.result.cache_valid(observation.after),
            "Fallback DNS result became cacheable",
        )?;
        cache.insert(observation.result, observation.after)?;
        require(
            cache.get(&target, observation.after).is_none(),
            "Fallback DNS result entered the actor cache",
        )?;
    }
    Ok(())
}

#[tokio::test]
async fn zero_ttl_results_keep_their_original_deadlines_without_becoming_cacheable(
) -> Result<(), ControlError> {
    for lookup in [Lookup::ExplicitPort, Lookup::ServiceFallback] {
        for family in [Family::Ipv4, Family::Ipv6] {
            let observation = resolve(lookup, family, 0).await?;
            let deadline = original_deadline(&observation, 0)?;
            require(
                !observation.result.cache_valid(deadline)
                    && !observation.result.cache_valid(observation.after),
                "Zero TTL answer became cacheable",
            )?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn cacheable_dns_results_expire_exactly_at_the_original_deadline() -> Result<(), ControlError>
{
    for family in [Family::Ipv4, Family::Ipv6] {
        let observation = resolve(Lookup::ExplicitPort, family, 60).await?;
        let deadline = original_deadline(&observation, 60)?;
        require(
            observation
                .result
                .cache_valid(deadline - Duration::from_nanos(1))
                && !observation.result.cache_valid(deadline),
            "DNS cache boundary changed",
        )?;
        let target = observation.result.target.clone();
        let mut cache = ResolutionCache::default();
        cache.insert(observation.result, observation.after)?;
        require(
            cache
                .get(&target, deadline - Duration::from_nanos(1))
                .is_some()
                && cache.get(&target, deadline).is_none(),
            "Actor cache changed the original DNS deadline",
        )?;
    }
    Ok(())
}

#[test]
fn literal_targets_have_no_dns_deadline_or_cache_entry() -> Result<(), ControlError> {
    for uri in ["sip:account@127.0.0.1:5070", "sip:account@[::1]:5070"] {
        let target = StaticTarget::from_uri(Protocol::Udp, uri).map_err(failure)?;
        let result = ResolvedTarget::literal(&target)?
            .ok_or(ControlError::Invalid("Numeric target was not literal"))?;
        let now = Instant::now();
        require(
            deadline(&result).is_none() && !result.cache_valid(now),
            "Literal target acquired DNS validity",
        )?;
        let mut cache = ResolutionCache::default();
        cache.insert(result, now)?;
        require(
            cache.get(&target, now).is_none(),
            "Literal target entered the DNS cache",
        )?;
    }
    Ok(())
}
