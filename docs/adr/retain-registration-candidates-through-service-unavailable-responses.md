---
date: 2026-11-17
status: proposed
---

# Keep registration candidates through service unavailable responses

## Context

[RFC 3263 §4.3](https://www.rfc-editor.org/rfc/rfc3263.html#section-4.3) recommends trying another resolved server after 503.
Registration routing keeps only the selected peer.

## Decision

Keep at most eight resolved candidates for User Datagram Protocol (UDP) registration.
Advance only after a checked 503.
Keep eight peer deadlines per client across success, exhaustion, and resolution changes.
Reserve capacity before peer selection.
Wait when full.
Never evict unexpired deadlines.
Preserve Request-URI, Call-ID, From-tag, and Contact as project policy.
Use fresh branches and increasing CSeq under [RFC 3261 §10.2](https://www.rfc-editor.org/rfc/rfc3261.html#section-10.2).
Keep original Domain Name System (DNS) deadlines and cacheability distinct from transaction deadlines.
Keep starting settings, configuration leases, and cancellation checks.
Preserve sixteen registrations, sixteen shared setups, and sixty-four connections.

## Alternatives

Retrying one peer ignores alternatives.
Resolving again can reorder candidates.
A shared overload table needs another ownership boundary.

## Consequences

Success can discard the cursor and unused reservations.
Retry deadlines end at expiry or client retirement.
Explicit 408, shared 503 suppression across clients, and reload remain separate.
Reconsider when DNS expiry changes selection or other transports enter traversal.

## Confirmation

Wire tests must observe 503, backup authentication, stable identity, fresh branches and CSeq, persistent peer waits, and cleanup after failure.
