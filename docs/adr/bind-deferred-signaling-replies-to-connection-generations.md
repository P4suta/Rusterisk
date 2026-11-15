---
date: 2026-11-16
status: proposed
---

# Bind deferred replies to connection generations

## Context

The peer socket address can identify a replacement connection before a delayed media effect arrives.
[RFC 3261 section 18.2.2](https://www.rfc-editor.org/rfc/rfc3261.html#section-18.2.2) requires connection association and recommends reconnecting after closure.

## Decision

Capture the current connection generation when a reliable response enters deferred storage.
Move the complete response and its transport route together.
Release a response only while the captured generation remains open.
Discard stale responses and end the affected call through the existing control path.
Keep datagram routing explicit in the same closed transport enum.
Suppress cached retries while the matching response remains pending.

## Alternatives

A peer address alone can route an old answer onto a replacement stream.
Reconnecting during media response delivery would outlive the call cleanup required after flow loss.
Holding a socket reference would split connection lifetime between adapters.

## Consequences

The network adapter remains the sole authority for connection generations.
Deferred values own bounded message bytes and contain no socket or shared domain state.

## Confirmation

Replace a connection at the same peer address before media acknowledgement.
Check that no old bytes reach the replacement and that current replies keep their delivery behavior.
