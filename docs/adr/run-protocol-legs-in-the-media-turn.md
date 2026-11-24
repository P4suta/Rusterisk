---
date: 2026-11-24
status: proposed
---

# Run protocol legs in the media turn

## Context

Each Real-time Transport Protocol leg ran a task with its own 20 millisecond clock.
Its two frame channels reserved about 124 kilobytes, and audio waited for both clocks.

## Decision

The media task owns each leg and runs it in its 20 millisecond turn.
Every leg first reads what arrived on its non-blocking sockets and hands one frame to the mixer.
After the mix, each leg sends its frame.
A leg reads at most 16 datagrams from each socket in a turn.
A failing leg ends alone with a stopped notice.

## Alternatives

Smaller channels keep the task, the timer, and the clock phase.
Waiting on each socket's readiness wakes the task for every packet.

## Consequences

A packet waits up to one turn before the media task reads it.
WebRTC and stream links keep their tasks until later changes move them.

## Confirmation

Tests must show that an attachment starts no task and that one failing leg leaves another's audio running.
