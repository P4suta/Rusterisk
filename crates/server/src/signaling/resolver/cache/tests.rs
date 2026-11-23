use super::ResolutionCache;
use crate::signaling::resolver::{Cacheability, ResolutionValidity, ResolvedTarget};
use crate::ControlError;
use rusterisk_config::Protocol;
use rusterisk_sip::StaticTarget;
use std::time::Duration;
use tokio::time::Instant;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn result(
    index: usize,
    until: Instant,
    caching: Cacheability,
) -> Result<ResolvedTarget, ControlError> {
    let identity = format!("peer-{index}.test");
    let target = StaticTarget::from_uri(Protocol::Tcp, &format!("sip:phone@{identity}:5070"))
        .map_err(failure)?;
    let mut peers = [None; 8];
    peers[0] = Some("127.0.0.1:5070".parse().map_err(failure)?);
    Ok(ResolvedTarget {
        target,
        identity,
        peers,
        validity: ResolutionValidity::Dns { until, caching },
    })
}

#[test]
fn fixed_slots_evict_before_insert_and_expire_at_the_record_deadline() -> Result<(), ControlError> {
    let now = Instant::now();
    let expires = now + Duration::from_secs(1);
    let mut cache = ResolutionCache::default();
    for index in 0..128 {
        cache.insert(result(index, expires, Cacheability::Reusable)?, now)?;
        assert_eq!(cache.entries.iter().flatten().count(), (index + 1).min(64));
    }
    assert!(cache
        .get(&result(0, expires, Cacheability::Reusable)?.target, now)
        .is_none());
    let last = result(127, now + Duration::from_secs(2), Cacheability::Reusable)?;
    let target = last.target.clone();
    cache.insert(last, now)?;
    assert_eq!(cache.entries.iter().flatten().count(), 64);
    assert!(cache.get(&target, expires).is_some());
    assert_eq!(cache.entries.iter().flatten().count(), 1);
    assert!(cache.get(&target, now + Duration::from_secs(2)).is_none());
    assert_eq!(cache.entries.iter().flatten().count(), 0);
    Ok(())
}

#[test]
fn uncacheable_fallbacks_and_expired_results_never_enter_the_actor_cache(
) -> Result<(), ControlError> {
    let now = Instant::now();
    let mut cache = ResolutionCache::default();
    for (until, caching) in [
        (now + Duration::from_secs(1), Cacheability::Uncacheable),
        (now, Cacheability::Reusable),
        (now - Duration::from_secs(1), Cacheability::Reusable),
    ] {
        let result = result(0, until, caching)?;
        let target = result.target.clone();
        cache.insert(result, now)?;
        assert!(cache.get(&target, now).is_none());
        assert_eq!(cache.entries.iter().flatten().count(), 0);
    }
    Ok(())
}
