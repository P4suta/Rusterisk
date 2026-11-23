---
date: 2026-11-24
status: proposed
---

# Wake each task at its earliest deadline

## Context

Control wakes every 100 ms and signaling and media every 20 ms, so an idle server fires 110 timers per second.
Each interval scans for deadlines, which fire up to one period late.

## Decision

Wake a task for an event or its earliest deadline.
Each state machine reports the earliest deadline it can act on as a pure value.
Find the earliest one after every turn and sleep until it.
Resume budgeted work in the same turn or at a stated pacing deadline.
Keep the media clock as the single periodic timer while media exists.

## Alternatives

Fixed intervals repair a missed deadline but cost every idle wakeup.
A timer queue needs cancellation and stale-entry checks that scanning 64 calls avoids.

## Consequences

An unreported deadline never fires.
Reconsider the scan if the channel limit grows.

## Confirmation

Tests and bounded proofs must show that advancing before a reported deadline changes nothing.
A differential test must match an engine driven every millisecond with one driven at its deadlines.
A [paused-time](https://docs.rs/tokio/latest/tokio/time/fn.pause.html) test must count zero loop turns in an idle hour.
