---
date: 2026-11-24
status: accepted
---

# Owe media orders while the media queue is full

## Context

The composed task sends releases, bridge changes, gates, and recording ends to the media task unanswered.
A full queue failed the send and stopped the server, and a hangup of many calls can fill it.
Waiting in the turn would hold every call's signaling behind the media task.

## Decision

Keep a refused order in an owed queue, and send later orders behind it.
Direction changes travel as orders, and an attachment fails while orders wait.
Retry pending orders at capacity.
The task waits for room only while it owes orders, between turns.
The owed queue holds one more media queue, and the server stops only when the media task stops taking orders.

## Alternatives

Waiting in the turn keeps the order but stalls signaling.
Dropping the order leaks media for an ended call.

## Consequences

A released call's media ends after the owed orders ahead of it.
A slot table can later hold these owed releases as call state confirmed by the media task.

## Confirmation

Tests must show that a hangup into a full queue keeps signaling responsive and sends its release in order.
