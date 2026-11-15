---
date: 2026-11-15
status: proposed
---

# Reserve client request capacity for terminal cleanup

## Context

Originate responses can remain pending while a caller cancels or observes Stasis entry.
Starting one request for every handle can exhaust server connections and delay cleanup.
Waiting for a media state request inside event reception can block its own notification.

## Decision

Bound each session to 16 active request workers.
Admit at most eight originate and media state workers together, reserving eight slots for hangup requests.
Reject new originate commands when foreground capacity is full.
Keep terminal cleanup pending in its call entry until a worker slot becomes available.
Read existing media state after Stasis entry through an owned worker.
Report a failed state request through the call snapshot and milestone wait.
Join all workers when the session closes.

## Alternatives

Unrestricted per-handle requests amplify load during cancellation.
Running requests inside event reception blocks unrelated progress.
A global mutable semaphore would add shared domain state.

## Consequences

Sessions still share the server's admission limits.
Reconsider worker budgets using measured setup and cleanup latency.

## Confirmation

Test saturated foreground admission, reserved cleanup, pending media requests, early media, failures, cancellation, and shutdown.
Keep the record proposed until integration checks pass.
