---
date: 2026-11-15
status: proposed
---

# Attach external streams through owned media links

## Context

The approved plan includes AudioSocket and external WebSocket audio.
[AudioSocket](https://docs.asterisk.org/Configuration/Channel-Drivers/AudioSocket/) carries framed audio over a byte stream.
Network admission must preserve control and signaling progress.

## Decision

Use a closed transport enum behind the existing bounded media port.
Move each connected socket into one owned link task.
Keep partial reads in bounded buffers across cancellation points.
Run connection attempts as bounded owned jobs and correlate completion with channel generation.
Expose acceptance, Stasis, and received audio as independent observations.

## Alternatives

Separate audio servers would duplicate routing and recording ownership.
Connecting inside the signaling loop would block unrelated calls until the connection deadline.
Shared sockets would complicate cancellation and require coordination across writers.

## Consequences

Transport variants share the mixer and cleanup path.
Stream framing and connection deadlines need fault tests.
Reconsider the boundary if a transport requires independent routing ownership.

## Confirmation

Tests cover fragmented messages, known audio in both directions, failed connection, cancellation, and late admission.
Reference comparisons verify AudioSocket framing and channel behavior.
The record stays proposed pending integration and review.
