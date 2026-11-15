---
date: 2026-11-16
status: proposed
---

# Resolve static targets in owned setup jobs

## Context

[RFC 3263](https://www.rfc-editor.org/rfc/rfc3263.html) locates signaling targets through address and service records.
[Hickory 0.26.2](https://docs.rs/hickory-resolver/0.26.2/hickory_resolver/) supplies concrete asynchronous lookups and fixes [unbounded retry behavior](https://rustsec.org/advisories/RUSTSEC-2026-0513.html).
Its [shared cache](https://docs.rs/moka/0.12.16/moka/sync/struct.Cache.html) bounds entries through deferred eviction.

## Decision

Use the exact resolver with system configuration.
Preserve nameservers, search domains, and host-file behavior.
Reject excess resolver settings.
Use the configured transport, service records without an explicit port, and address records otherwise.
Bound each query to 500 milliseconds, resolution to two seconds, setup jobs to 16, and returned candidates to eight.
Turn off library caching.
Keep at most 64 actor results.
Evict before insertion and use the earliest record expiry.
Count pending and cancelling setup jobs within 64 connection slots until joined.
Keep sockets inside setup tasks until admission confirms the live call generation and control lifetime.
Send no signaling bytes or media before admission.
Verify encrypted peer identity using the original URI host.

## Alternatives

[Blocking lookup timeouts](https://docs.rs/tokio/1.54.1/src/tokio/net/addr.rs.html) leave work running beyond adapter cancellation.

## Consequences

Registered browser flows bypass host lookup.
Never cache a service-record fallback after a negative answer.

## Confirmation

Exercise wire limits, records, cache, deadlines, cancellation, stale generations, trust, and shutdown with local peers.
