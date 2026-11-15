---
date: 2026-11-15
status: proposed
---

# Confine client TLS sharing to immutable library config

## Context

Initial encrypted signaling connections require certificate and peer-name verification.
The pinned [Tokio Rustls client API](https://raw.githubusercontent.com/rustls/tokio-rustls/v/0.26.6/src/client.rs) accepts only `Arc<ClientConfig>` and provides no owned configuration factory.
Axum provides no client factory.

## Decision

Use native trust roots or a bounded configured certificate list.
Keep certificate and peer-name verification mandatory.
Permit one fully qualified `Arc<ClientConfig>` constructor in the registered `client_connector` function.
Keep the lint expectation on the constructor statement and register its exact file, function, lint, reason, and decision.
Inspect the constructor type and placement in the source gate and keep resolved-type Clippy enforcement.
Expose only the concrete library connector outside the helper.
Keep sockets, tasks, cancellation, and domain state owned.

## Alternatives

Rejecting outbound TLS would leave the approved native transport contract incomplete.
Hiding conversion through aliases would weaken the source policy.
A function-wide exception would admit unrelated shared state.

## Consequences

The dependency shares immutable external configuration internally.
Reconsider the exception when the dependency offers an owned factory.

## Confirmation

Reject changed types, aliases, placement, duplicate constructors, missing registration, and changed reasons.
Verify trusted connections and reject untrusted certificates and mismatched names with real TLS handshakes.
