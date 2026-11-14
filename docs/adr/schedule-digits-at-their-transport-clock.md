---
date: 2026-11-15
status: proposed
---

# Schedule digits at their transport clock

## Context

Advancing digit sequences before media admission can fill a packet queue after control accepts the request.
[Telephone event timing](https://datatracker.ietf.org/doc/html/rfc4733#section-2.5.1.2) must describe the actual event start and duration.

## Decision

Move complete accepted sequences to the owned media task.
Advance the core sequence when its event sender can start the next digit.
Release admission after every digit and the final pause finish.
Signaling schedules digits sent through `SIP INFO`.

## Alternatives

The coordinator could keep sequences and receive acknowledgements for each digit's start.
That preserves one scheduler but adds messages and correlation state for every digit.
Increasing packet capacity postpones saturation without keeping completion accurate.

## Consequences

Control admits eight commands per call.
Media scheduling stays bounded and ends with the call's transport.
Reconsider placement if integration measurements show missed audio deadlines.

## Confirmation

Receive every digit from consecutive short sequences and verify order, duration, cancellation, and completion admission.
Reference timing and load measurements remain required before acceptance.
