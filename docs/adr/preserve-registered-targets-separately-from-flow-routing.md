---
date: 2026-11-16
status: proposed
---

# Preserve registered targets and flow routing

## Context

[RFC 3261 section 10.2.1](https://www.rfc-editor.org/rfc/rfc3261.html#section-10.2.1) registers Contact addresses as request targets.
[RFC 7118 appendix B.1](https://www.rfc-editor.org/rfc/rfc7118.html#appendix-B.1) permits browser Contact hosts ending in `.invalid` because established connections carry requests.
Rebuilding a target from an endpoint name and socket address loses the registered identity.

## Decision

Store each accepted registration as an owned Contact URI, observed peer address, and expiry.
Use the URI for outbound Request-URI and the observed peer for existing transport routing.
Refresh or remove the complete binding atomically.
Preserve the existing single-binding endpoint contract and bounded registration count.
Expire bindings and remove disconnected peers before later calls select a target.

## Alternatives

Reconstructing a URI discards browser identities and URI parameters.
Resolving registered Contact hosts would fail for browser placeholders and bypass established flow routing.

## Consequences

The transport adapter continues to own connection generations.
Registered targets never cause host lookups for browser placeholders.
Extra bindings and the `SIP Outbound` extensions require a separate compatibility decision.

## Confirmation

Exercise authenticated registration, refreshed random Contact URIs, original physical routing, expiry, and disconnect cleanup.
