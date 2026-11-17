---
date: 2026-11-17
status: proposed
---

# Keep registration candidates through service unavailable responses

## Context

[RFC 3263 §4.3](https://www.rfc-editor.org/rfc/rfc3263.html#section-4.3) recommends trying another resolved server after 503.
Registration routing discarded unused resolved peers.

## Decision

Limit User Datagram Protocol (UDP) candidates and peer deadlines to eight each per client.
Advance cursors only after a checked 503.
Reserve capacity before peer selection.
Wait for the earliest deadline when full.
Never evict unexpired deadlines.
Preserve Request-URI, Call-ID, From-tag, and Contact as project policy.
Use fresh branches and increasing CSeq under [RFC 3261 §10.2](https://www.rfc-editor.org/rfc/rfc3261.html#section-10.2).
Keep original Domain Name System (DNS) deadlines and cacheability distinct from transaction deadlines.
Keep the failed `RegistrationResponse` during fresh DNS under starting settings, configuration leases, and cancellation.
Preserve sixteen registrations, sixteen shared setups, and sixty-four connections.

## Alternatives

One peer ignores alternatives.
Rebuilding requests can change From-tag.
Shared suppression needs another ownership boundary.

## Consequences

Success releases unused reservations while retaining unexpired deadlines.
Retry deadlines survive exhaustion and resolution until expiry or client retirement.
Explicit 408, shared 503 suppression across clients, and reload remain separate.
Reconsider when DNS expiry changes selection or other transports enter traversal.

## Confirmation

Wire tests must observe 503, backup authentication, stable identity, fresh branches and CSeq, persistent peer waits, and cleanup after failure.
