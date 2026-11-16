use super::{DestinationResolver, ResolutionCache};
use crate::ControlError;
use hickory_resolver::{
    config::{LookupIpStrategy, NameServerConfig, ResolveHosts, ResolverConfig, ResolverOpts},
    proto::{
        op::{Message, OpCode, Query},
        rr::{
            rdata::{A, AAAA, SRV},
            Name, RData, Record, RecordType,
        },
    },
};
use rusterisk_config::Protocol;
use rusterisk_sip::StaticTarget;
use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    time::{Duration, Instant},
};
use tokio::{net::UdpSocket, sync::watch, task::JoinHandle};
mod validity;
mod wire;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

struct DnsFixture {
    address: SocketAddr,
    stop: watch::Sender<bool>,
    job: JoinHandle<Result<Vec<Query>, ControlError>>,
}

#[tokio::test]
async fn short_record_lifetimes_are_never_extended_by_resolver_options() -> Result<(), ControlError>
{
    let fixture = DnsFixture::start(|query| {
        Ok(std::iter::once(Record::from_rdata(
            query.name().clone(),
            1,
            RData::A(A(Ipv4Addr::LOCALHOST)),
        ))
        .collect())
    })
    .await?;
    let mut options = ResolverOpts::default();
    options.ip_strategy = LookupIpStrategy::Ipv4Only;
    options.positive_min_ttl = Some(Duration::from_secs(3600));
    let resolver = fixture.resolver(options)?;
    let target =
        StaticTarget::from_uri(Protocol::Tcp, "sip:phone@peer.test:5070").map_err(failure)?;
    let result = resolver.resolve(&target).await?;
    assert!(!result.cache_valid(Instant::now() + Duration::from_secs(2)));
    assert_eq!(fixture.finish().await?.len(), 1);
    Ok(())
}

impl DnsFixture {
    async fn start<F>(mut answer: F) -> Result<Self, ControlError>
    where
        F: FnMut(&Query) -> Result<Vec<Record>, ControlError> + Send + 'static,
    {
        let socket = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
        let address = socket.local_addr().map_err(failure)?;
        let (stop, mut stopped) = watch::channel(false);
        let job = tokio::spawn(async move {
            let mut bytes = vec![0; 65536];
            let mut requests = Vec::with_capacity(512);
            loop {
                tokio::select! {
                    _changed = stopped.changed() => return Ok(requests),
                    packet = socket.recv_from(&mut bytes) => {
                        let (size, peer) = packet.map_err(failure)?;
                        let message = Message::from_vec(&bytes[..size]).map_err(failure)?;
                        let query = message.queries.first().ok_or(ControlError::Capacity)?.clone();
                        if requests.len() == 512 { return Err(ControlError::Capacity); }
                        requests.push(query.clone());
                        let mut reply = Message::response(message.metadata.id, OpCode::Query);
                        reply.metadata.recursion_available = true;
                        reply.metadata.recursion_desired = message.metadata.recursion_desired;
                        reply.metadata.authoritative = true;
                        reply.queries.push(query.clone());
                        reply.answers = answer(&query)?;
                        reply.edns = message.edns;
                        let reply = reply.to_vec().map_err(failure)?;
                        let sent = socket.send_to(&reply, peer).await.map_err(failure)?;
                        if sent != reply.len() { return Err(ControlError::Invalid("Incomplete resolver fixture response")); }
                    },
                }
            }
        });
        Ok(Self { address, stop, job })
    }

    fn resolver(&self, mut options: ResolverOpts) -> Result<DestinationResolver, ControlError> {
        options.use_hosts_file = ResolveHosts::Never;
        let mut server = NameServerConfig::udp(self.address.ip());
        server.connections[0].port = self.address.port();
        DestinationResolver::with_config(
            ResolverConfig::from_name_servers(std::iter::once(server).collect()),
            options,
        )
    }

    async fn finish(self) -> Result<Vec<Query>, ControlError> {
        self.stop.send_replace(true);
        self.job.await.map_err(failure)?
    }
}

#[tokio::test]
async fn explicit_ports_resolve_both_address_families_and_reuse_bounded_cache(
) -> Result<(), ControlError> {
    let fixture = DnsFixture::start(|query| {
        let data = if query.query_type() == RecordType::A {
            RData::A(A(Ipv4Addr::LOCALHOST))
        } else if query.query_type() == RecordType::AAAA {
            RData::AAAA(AAAA(Ipv6Addr::LOCALHOST))
        } else {
            return Err(ControlError::Invalid(
                "Unexpected service lookup for explicit port",
            ));
        };
        Ok(std::iter::once(Record::from_rdata(query.name().clone(), 60, data)).collect())
    })
    .await?;
    let resolver = fixture.resolver(ResolverOpts::default())?;
    let target =
        StaticTarget::from_uri(Protocol::Tcp, "sip:phone@peer.test:5070").map_err(failure)?;
    let mut cache = ResolutionCache::default();
    for _ in 0..2 {
        let result = match cache.get(&target, Instant::now()) {
            Some(result) => result,
            None => {
                let result = resolver.resolve(&target).await?;
                cache.insert(result.clone(), Instant::now())?;
                result
            }
        };
        assert_eq!(result.identity(), "peer.test");
        let peers: Vec<_> = result.peers().collect();
        assert_eq!(peers.len(), 2);
        assert!(peers.contains(&"127.0.0.1:5070".parse().map_err(failure)?));
        assert!(peers.contains(&"[::1]:5070".parse().map_err(failure)?));
    }
    assert_eq!(fixture.finish().await?.len(), 2);
    Ok(())
}

#[tokio::test]
async fn resolver_options_preserve_system_semantics_and_reject_excess_configuration(
) -> Result<(), ControlError> {
    let domain = Name::from_ascii("local.test.").map_err(failure)?;
    let config = ResolverConfig::from_parts(
        Some(domain.clone()),
        std::iter::once(domain.clone()).collect(),
        std::iter::once(NameServerConfig::udp(Ipv4Addr::LOCALHOST.into())).collect(),
    );
    let mut options = ResolverOpts::default();
    options.ndots = 4;
    options.use_hosts_file = ResolveHosts::Always;
    options.ip_strategy = LookupIpStrategy::Ipv4Only;
    options.timeout = Duration::from_secs(90);
    options.cache_size = 8192;
    options.attempts = 10;
    options.num_concurrent_reqs = 20;
    options.max_active_requests = 1024;
    let resolver = DestinationResolver::with_config(config, options)?;
    let options = resolver.inner.options();
    assert_eq!(options.ndots, 4);
    assert_eq!(options.use_hosts_file, ResolveHosts::Always);
    assert_eq!(options.ip_strategy, LookupIpStrategy::Ipv4Only);
    assert_eq!(options.timeout, Duration::from_millis(500));
    assert_eq!(options.cache_size, 0);
    assert_eq!(options.attempts, 1);
    assert_eq!(options.num_concurrent_reqs, 1);
    assert_eq!(options.max_active_requests, 16);
    assert_eq!(options.edns_payload_len, 1232);
    let excessive_servers =
        ResolverConfig::from_name_servers(vec![
            NameServerConfig::udp(Ipv4Addr::LOCALHOST.into());
            9
        ]);
    assert!(matches!(
        DestinationResolver::with_config(excessive_servers, ResolverOpts::default()),
        Err(ControlError::Capacity)
    ));
    let excessive_search = ResolverConfig::from_parts(
        None,
        vec![domain; 9],
        std::iter::once(NameServerConfig::udp(Ipv4Addr::LOCALHOST.into())).collect(),
    );
    assert!(matches!(
        DestinationResolver::with_config(excessive_search, ResolverOpts::default()),
        Err(ControlError::Capacity)
    ));
    Ok(())
}

#[tokio::test]
async fn address_answers_beyond_eight_are_rejected_before_returning_candidates(
) -> Result<(), ControlError> {
    let fixture = DnsFixture::start(|query| {
        Ok((1..=9)
            .map(|value| {
                Record::from_rdata(
                    query.name().clone(),
                    60,
                    RData::A(A(Ipv4Addr::new(127, 0, 0, value))),
                )
            })
            .collect())
    })
    .await?;
    let mut options = ResolverOpts::default();
    options.ip_strategy = LookupIpStrategy::Ipv4Only;
    let resolver = fixture.resolver(options)?;
    let target =
        StaticTarget::from_uri(Protocol::Udp, "sip:phone@peer.test:5070").map_err(failure)?;
    assert!(matches!(
        resolver.resolve(&target).await,
        Err(ControlError::Capacity)
    ));
    fixture.finish().await?;
    Ok(())
}

#[tokio::test]
async fn service_records_preserve_original_identity_and_order_target_priorities(
) -> Result<(), ControlError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let fixture = DnsFixture::start(|query| {
            let records = if query.query_type() == RecordType::SRV {
                [
                    Record::from_rdata(
                        query.name().clone(),
                        60,
                        RData::SRV(SRV::new(
                            10,
                            1,
                            5090,
                            Name::from_ascii("backup.test.").map_err(failure)?,
                        )),
                    ),
                    Record::from_rdata(
                        query.name().clone(),
                        60,
                        RData::SRV(SRV::new(
                            0,
                            1,
                            5080,
                            Name::from_ascii("primary.test.").map_err(failure)?,
                        )),
                    ),
                ]
                .into_iter()
                .collect()
            } else if query.query_type() == RecordType::A {
                std::iter::once(Record::from_rdata(
                    query.name().clone(),
                    60,
                    RData::A(A(if query.name().to_ascii() == "primary.test." {
                        Ipv4Addr::new(127, 0, 0, 1)
                    } else {
                        Ipv4Addr::new(127, 0, 0, 2)
                    })),
                ))
                .collect()
            } else if query.query_type() == RecordType::AAAA {
                Vec::new()
            } else {
                return Err(ControlError::Invalid("Unexpected service fixture query"));
            };
            Ok(records)
        })
        .await?;
        let mut options = ResolverOpts::default();
        options.ip_strategy = LookupIpStrategy::Ipv4Only;
        let resolver = fixture.resolver(options)?;
        let target = StaticTarget::from_uri(
            protocol,
            if protocol == Protocol::Tls {
                "sips:phone@peer.test"
            } else {
                "sip:phone@peer.test"
            },
        )
        .map_err(failure)?;
        let result = resolver.resolve(&target).await?;
        assert_eq!(result.identity(), "peer.test");
        assert_eq!(
            result.peers().collect::<Vec<_>>(),
            [
                "127.0.0.1:5080".parse::<SocketAddr>().map_err(failure)?,
                "127.0.0.2:5090".parse().map_err(failure)?
            ]
        );
        let queries = fixture.finish().await?;
        assert_eq!(
            queries
                .first()
                .ok_or(ControlError::Capacity)?
                .name()
                .to_ascii(),
            if protocol == Protocol::Tls {
                "_sips._tcp.peer.test."
            } else {
                "_sip._tcp.peer.test."
            }
        );
    }
    Ok(())
}

#[tokio::test]
async fn absent_service_records_fall_back_but_explicit_unavailability_never_does(
) -> Result<(), ControlError> {
    for unavailable in [false, true] {
        let fixture = DnsFixture::start(move |query| {
            if query.query_type() == RecordType::SRV && unavailable {
                Ok(std::iter::once(Record::from_rdata(
                    query.name().clone(),
                    60,
                    RData::SRV(SRV::new(0, 0, 0, Name::root())),
                ))
                .collect())
            } else if query.query_type() == RecordType::SRV {
                Ok(Vec::new())
            } else if query.query_type() == RecordType::A {
                Ok(std::iter::once(Record::from_rdata(
                    query.name().clone(),
                    60,
                    RData::A(A(Ipv4Addr::LOCALHOST)),
                ))
                .collect())
            } else {
                Err(ControlError::Invalid("Unexpected service fallback query"))
            }
        })
        .await?;
        let mut options = ResolverOpts::default();
        options.ip_strategy = LookupIpStrategy::Ipv4Only;
        let resolver = fixture.resolver(options)?;
        let target =
            StaticTarget::from_uri(Protocol::Udp, "sip:phone@peer.test").map_err(failure)?;
        let result = resolver.resolve(&target).await;
        if unavailable {
            assert!(matches!(result, Err(ControlError::NotFound(_))));
            assert_eq!(fixture.finish().await?.len(), 1);
        } else {
            assert_eq!(
                result?.peers().collect::<Vec<_>>(),
                ["127.0.0.1:5060".parse::<SocketAddr>().map_err(failure)?]
            );
            let queries = fixture.finish().await?;
            assert_eq!(queries.len(), 2);
            assert_eq!(queries[0].name().to_ascii(), "_sip._udp.peer.test.");
        }
    }
    Ok(())
}

#[test]
fn service_weights_follow_inclusive_selection_and_keep_lower_priorities_last(
) -> Result<(), ControlError> {
    let record = |priority, weight, port| SRV::new(priority, weight, port, Name::root());
    let records = [
        record(10, 1, 4),
        record(0, 0, 1),
        record(0, 1, 2),
        record(0, 3, 3),
    ]
    .into_iter()
    .collect();
    let mut draws = [0, 4, 1, 0].into_iter();
    let ordered = super::ordered_srv(records, |bound| draws.next().unwrap_or(bound))?;
    assert_eq!(
        ordered.iter().map(|record| record.port).collect::<Vec<_>>(),
        [1, 3, 2, 4]
    );
    assert!(matches!(
        super::ordered_srv(vec![record(0, 1, 1); 9], |_| 0),
        Err(ControlError::Capacity)
    ));
    assert!(matches!(
        super::ordered_srv(std::iter::once(record(0, 1, 1)).collect(), |_| 2),
        Err(ControlError::Invalid(_))
    ));
    Ok(())
}
