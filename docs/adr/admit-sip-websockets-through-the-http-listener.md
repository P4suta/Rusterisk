---
date: 2026-11-15
status: proposed
---

# Reserve signaling upgrades through the web listener

## Context

[Asterisk](https://docs.asterisk.org/Configuration/WebRTC/Configuring-Asterisk-for-WebRTC-Clients/) serves signaling WebSockets at the HTTP `/ws` endpoint and configures their encryption in `http.conf`.
[RFC 7118](https://www.rfc-editor.org/rfc/rfc7118.html) requires the `sip` subprotocol and preserves message boundaries.

## Decision

Reuse the HTTP listener, encryption, and configured Origin policy.
Reserve a signaling connection generation before accepting an upgrade.
Move upgraded sockets through bounded admission into owned signaling jobs.
Expire abandoned reservations and reject requests beyond capacity.
Authenticate signaling identities through existing digest challenges.
Accept text and binary messages with exactly one complete signaling message.
Accept messages without `Content-Length` and reject ambiguous lengths.
Preserve ordered input and completion, reliable transaction deadlines, and bounded shutdown.
Send outbound WebSocket signaling through existing registered flows.

## Alternatives

Independent signaling listeners would duplicate encryption and contradict Asterisk configuration.
Unreserved upgrades would move connection pressure outside signaling capacity.
Byte-stream decoding would lose WebSocket message boundaries.

## Consequences

Transport metadata depends on the available HTTP scheme.
Origin policy changes require restart for configured signaling upgrades.
Reconsider routing when the HTTP service supports independent listeners.

## Confirmation

Exercise native upgrades, digest registration, calls, malformed boundaries, Origin rejection, capacity, cancellation, and listener release.
