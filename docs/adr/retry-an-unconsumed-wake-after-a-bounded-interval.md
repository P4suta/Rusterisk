---
date: 2026-11-24
status: accepted
---

# Retry an unconsumed wake after a bounded interval

## Context

The control task sleeps until the earliest wake that its state reports.
Backpressure can leave due work that a turn can't consume, such as an expiry that the pending effect budget defers.
Sleeping on that wake spins, and stopping the timer hides every later deadline.

## Decision

When a turn leaves a due wake unchanged, wake again after 100 ms.
Report no wake for work that an event alone ends, such as a cancelled reload with a running loader.

## Alternatives

Stopping the timer until the next event hid later deadlines in review.
Tracking each wake source keeps other deadlines exact but needs an identity for every source.
The fixed interval costs every idle turn.

## Consequences

Other deadlines can fire up to 100 ms late while such work exists, as they did under the interval.
The retry can mask an inexact wake report, so the task logs each new one.
Reconsider when retries appear without backpressure.

## Confirmation

Tests must show that an unconsumed due wake retries after one interval.
Tests must show that a cancelled reload with a running loader reports no wake.
