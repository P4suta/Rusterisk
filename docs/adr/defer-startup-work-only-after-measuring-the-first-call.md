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

Deferring resolver construction until the first outbound lookup used 4,316 KiB of anonymous memory and 14 context switches per minute with a registered handset and one subscribed application.
Eager construction used 4,228 KiB and 13 switches under the same conditions.
First-call bidirectional audio took 1,431 and 1,292 ms across two fresh starts with the deferred resolver and 1,215 and 1,202 ms with eager construction.
Keep eager resolver construction.
