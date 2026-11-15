---
date: 2026-11-15
status: proposed
---

# Confine library sharing to protocol adapters

## Context

The approved dependencies contain sharing and dynamic dispatch.

## Decision

Use rsipstack 0.8.5 parsing and digest computation.
Avoid its endpoint's unbounded incoming channel.
Limit Tokio sharing to bounded transport and Axum sharing to immutable routing.

Use the concrete rtcp-types writers and borrowed packet enum.
Avoid `CompoundBuilder`, which stores boxed trait objects.
Keep webrtc-srtp dispatch inside an owned context per stream and direction with replay protection.
Bound each direction to four authenticated stream contexts.
Publish a stream slot only after authentication succeeds.
Use opus 0.4's bundled libopus through opusic-sys.
Use the borrowed Opus repacketizer only to construct bounded test packets.

## Alternatives

The complete endpoint introduces an unbounded queue.
Custom cryptography or codecs expand security and interoperability work.

## Consequences

Adapters supply timers, admission limits, and shutdown.
Reconsider dependencies that prevent those properties.

## Confirmation

Source inspection found the queue in `transaction/endpoint.rs` and cipher dispatch in `context/mod.rs`.
Protocol, replay, overload, and shutdown tests must confirm the executed paths before acceptance.
