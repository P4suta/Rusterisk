---
date: 2026-11-15
status: proposed
---

# Preserve terminal effects under control backpressure

## Context

A full event queue disconnects the app and ends its calls.
The signaling queue can be full when those cleanup effects become necessary.

## Decision

Check each control request's effect budget before changing state.
Keep an owned pending queue for effects that become necessary after admission.
Preserve effect order.
Suspend new call admission, writes, and signaling input while pending effects await delivery.
Continue authorization, reads, subscription cleanup, and terminal timers.

Bound pending effects to 129: two terminal effects for each of 64 calls plus one effect from the triggering signal.
Closed subscriptions can't increase the admitted call count while cleanup remains pending.
Global shutdown cancels remaining effects when the signaling receiver closes, because the owned signaling and media tasks perform their shutdown cleanup.

## Alternatives

Dropping terminal effects loses socket cleanup.
Waiting for capacity inside event publication can deadlock signaling admission.

## Consequences

Requests can receive a capacity error during cleanup.
Revisit the bound whenever a transition gains effects.

## Confirmation

Tests must fill both queues, preserve cleanup order, verify resumed admission, and check shutdown.
Keep the record proposed until full call integration passes.
