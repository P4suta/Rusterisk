---
date: 2026-11-15
status: superseded
---

# Preserve terminal effects under control backpressure

## Context

An event queue overflow ends the app's calls while the signaling queue may be full.

## Decision

[Reserve terminal cleanup behind each turn's effects](reserve-terminal-cleanup-behind-each-turns-effects.md) supersedes this record.
Check each request's effect budget before changing state.
Keep ordered, owned pending effects.
Suspend admission, writes, and signaling input until pending effects drain.
Continue authorization, reads, subscription cleanup, and terminal timers.
Spend remaining capacity on expired recording admissions after call cleanup.
Cancel late recording admission before publishing its start event.

Limit channels to 64 and pending effects to 129.
Reserve two pending effects per live channel before recording cleanup.
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
