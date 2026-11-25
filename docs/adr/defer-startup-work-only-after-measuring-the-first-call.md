---
date: 2026-11-26
status: proposed
---

# Defer startup work only after measuring the first call

## Context

Opening storage and starting adapters can allocate memory before a call arrives.

## Decision

Compare one deferred operation at a time with eager startup under the same CPU limit and settings.
Use the interval from the first call request until both peers hear the expected tone.

## Alternatives

Eager startup reports configuration and storage errors before accepting calls.

## Consequences

Retain eager startup for operations needed to establish readiness.
Defer optional work only if resident memory falls and first-call audio arrives within the baseline time.

## Confirmation

Measure anonymous resident pages before any call and the last of 64 disconnects against the 20 ms budget.
Repeat after changing startup dependencies.
