---
date: 2026-11-15
status: proposed
---

# Bound WebSocket media queues by message admission

## Context

[Asterisk 22.9](https://github.com/asterisk/asterisk/blob/22.9.0/channels/chan_websocket.c) queues audio and completion markers and signals backpressure at 900 frames.
The approved plan requires owned buffers and explicit capacity limits.

## Decision

Keep inline frames in one owned queue with at most 1000 audio frames and 128 markers.
Signal backpressure at 900 frames and resume below 800.
Validate complete message admission before changing the queue.
Register one `large_enum_variant` expectation for the frame-or-marker item.

## Alternatives

Heap indirection would shrink queue slots but conflict with the ownership policy.
Separate frame and marker queues would need another ordering counter and coordinated cleanup.
Unrestricted markers or message overshoot would weaken the capacity bound.

## Consequences

Oversize admissions preserve queued audio and report capacity failure.
The hard bound may reject bursts the reference accepts beyond its nominal limit.
Queue flush discards pending completion markers.
Reconsider the bound if measured workloads need larger admitted bursts.

## Confirmation

Tests cover framing, marker order, backpressure, pause, flush, and failed admission.
Reference comparisons check ordinary command and audio behavior.
The record stays proposed pending integration and review.
