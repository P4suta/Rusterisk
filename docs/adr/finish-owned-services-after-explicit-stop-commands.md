---
date: 2026-11-16
status: proposed
---

# Finish owned services after explicit stop commands

## Context

An external runtime can own a service independently of its client process.
A successful shutdown command doesn't prove that the owned client exited successfully.

## Decision

Consume `RunningCommand` when requesting shutdown through a bounded command.
Run the request under the existing supervisor and preserve interruption.
After request success, wait within the service deadline for its actual zero exit.
Send no extra graceful signal while awaiting that result.
Stop the owned process group on request failure, timeout, interruption, or failed completion.
Keep primary and cleanup failures together.
Leave external container removal to the runtime adapter's ownership checks.

## Alternatives

Signal proxying depends on the runtime client forwarding the signal.
Unsupervised shutdown commands lose group ownership and interruption handling.

## Consequences

The method consumes the service capability and borrows the existing supervisor.
No shared state or worker thread is necessary.
The existing signal-based stop remains available.

## Confirmation

Check successful shutdown, nonzero request and service exits, timeout, interruption, invalid budgets, and released listeners.
Keep proposed until the exported image passes actual shutdown and SDK verification.
