---
date: 2026-11-15
status: proposed
---

# Keep client events independent of call waits

## Context

Asterisk events can precede an originate response.
A cancelled request can still create a remote channel.

## Decision

Establish the event WebSocket before accepting originate requests.
Give one owned driver the socket, bounded commands, call lifetimes, and progress notifications.
Run bounded HTTP workers independently of event reception.
Track independent milestones for request acceptance, Stasis entry, answer, and media connection.
Cancel owned channels when their handles or session end, including late originate responses.

Use reqwest 0.13.5 and tokio-tungstenite 0.29.0 as concrete transport adapters.
Confine their internal sharing and type erasure to HTTP, TLS, and WebSocket machinery.
Bound response bodies, message sizes, pending calls, deadlines, and worker counts.

## Alternatives

Reading events inside a call wait can stall unrelated calls.
Reimplementing HTTP and TLS removes mature protocol handling without improving domain ownership.

## Consequences

Explicit shutdown waits for cleanup.
Drop initiates cleanup while the Tokio runtime remains available.
Reconsider the adapters if their lifecycle prevents bounded shutdown.

## Confirmation

The [reqwest builder](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html) supplies deadlines and redirect control.
Source inspection found `Arc<ClientRef>` in reqwest and TLS sharing in tokio-tungstenite.
Race, cancellation, overflow, and real socket tests must pass before acceptance.
