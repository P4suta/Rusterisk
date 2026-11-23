---
date: 2026-11-24
status: proposed
---

# Read one monotonic clock at task boundaries

## Context

Control and signaling each create a `std::time::Instant` epoch, and other tasks read further clocks.
Neither task can hold a deadline for the other, so both track the originate timeout.
[Paused Tokio time](https://docs.rs/tokio/latest/tokio/time/fn.pause.html) can't move these clocks, so tests wait in real time.

## Decision

Create one clock at startup from a `tokio::time::Instant` epoch and copy it into every task.
Read it when a turn starts and pass elapsed milliseconds to the core and the signaling engine as a value.
Store every deadline on that epoch.
Read wall-clock time through one registered function and format it at serialization.
Packet sample clocks [stay separate](keep-telephone-event-clocks-distinct.md).

## Alternatives

Per-task epochs keep the duplicated deadline.
An injected clock trait threads a parameter through every task while Tokio's timers follow another clock.

## Consequences

Timer tests need the current-thread runtime and in-memory transports.
Reconsider the epoch if a deadline must survive a restart.

## Confirmation

A paused-time test must fire an originate timeout by advancing virtual time.
The lint configuration must reject other monotonic and wall-clock reads once the last one goes.
