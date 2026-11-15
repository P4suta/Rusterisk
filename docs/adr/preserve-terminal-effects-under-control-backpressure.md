---
date: 2026-11-15
status: proposed
---

# Preserve terminal effects under control backpressure

## Context

An event queue overflow ends the app's calls while the signaling queue may be full.

## Decision

Check each request's effect budget before changing state.
Keep ordered, owned pending effects.
Suspend admission, writes, and signaling input until pending effects drain.
Continue authorization, reads, subscription cleanup, and terminal timers.

Limit pending effects to 129.
Each of 64 admitted channels reserves two effects across pending protocol work and cleanup.
A released branch spends its reservation.
A connected pair covers a bridge update, bridge removal, and two channel releases.
One extra effect covers the triggering signal.
Global shutdown cancels pending effects when the owned signaling scope closes.

## Alternatives

Dropping terminal effects loses socket cleanup.
Waiting for capacity inside event publication can deadlock signaling admission.

## Consequences

Requests can receive a capacity error during cleanup.
Revisit the bound whenever a transition gains effects.

## Confirmation

Tests fill both queues, preserve cleanup order, verify resumed admission, and check shutdown.
Keep the record proposed until reference call comparisons pass.
