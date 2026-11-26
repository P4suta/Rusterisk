---
date: 2026-11-24
status: accepted
---

# Reserve terminal cleanup behind each turn's effects

## Context

The composed task applies every effect of an input before it waits again, so effects no longer wait behind a full channel.
[Preserve terminal effects under control backpressure](preserve-terminal-effects-under-control-backpressure.md) suspended work until a pending queue drained.

## Decision

One queue holds a turn's effects in two tiers.
Ordinary work may fill 128 places, and each request compares its effect budget with them before it changes state.
Terminal cleanup of all 64 calls fits in 193 places behind them.
Ordinary work pauses only while cleanup spills past the ordinary tier.
The control loop without signaling hands every queued effect to its channel before its next input.

## Alternatives

One shared tier lets ordinary work take the places that cleanup needs.

## Consequences

A request can meet a capacity error only within a turn that ordinary work filled.
The public interface ends a turn after each request, so unit tests fill a turn.

## Confirmation

Tests must show that cleanup after an event overflow fits behind a full ordinary tier.
