---
date: 2026-11-16
status: proposed
---

# Advance output audio on the sample timeline

## Context

The mixer produces twenty-millisecond audio frames.
Using socket-task execution time for timestamps introduces scheduling variation into sample blocks.
[RFC 3550 section 5.1](https://www.rfc-editor.org/rfc/rfc3550.html#section-5.1) distinguishes sampling time from processing delay and describes a virtual presentation timeline for stored media.

## Decision

Keep a presentation clock in each audio transport.
Place frames on twenty-millisecond slots and advance one slot per accepted frame.
Use a later elapsed-time slot after silence, preserving omitted intervals.
Reject backwards time and arithmetic exhaustion before advancing the clock.
Validate frame duration before constructing a packet.
Keep explicit capture-time packet construction available for other callers.

## Alternatives

Raw execution time reproduces the observed timestamp variation.
A sample counter without elapsed-time alignment erases silent intervals.
Propagating capture timestamps through every media adapter would widen the change without giving stored playback an independent sampling clock.

## Consequences

Audio timestamps preserve sample continuity despite delivery jitter.
Telephone events keep the same clock origin and negotiated rate.
Missed presentation slots remain observable gaps.
Revisit for variable frame durations.

## Confirmation

Require deterministic jitter, silence, wrap, invalid-frame, and exhaustion cases, then repeat received-audio and digit tests.
