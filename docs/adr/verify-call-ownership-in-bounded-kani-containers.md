---
date: 2026-11-15
status: proposed
---

# Verify call ownership in bounded Kani containers

## Context

The approved plan requires Kani 0.68 checks for terminal states and resource ownership.
[Kani](https://model-checking.github.io/kani/usage.html) checks Rust harnesses and reports failed unwinding assertions.

## Decision

Point an independent, dependency-free proof manifest at the production core source.
Use six bounded harnesses for terminal transitions, connection claims, Stasis, bridge membership, task cancellation, and generation isolation.
Keep unwinding assertions and explicit reachability checks.
Run one verification worker in a digest-selected container with two processors, 2 GiB of memory, 256 processes, and a fifteen-minute deadline.
Reject missing harnesses, incomplete results, failed checks, unreachable cover goals, and unapproved verifier versions.
Read and delete Kani's temporary native result file without creating a separate evidence record.

## Alternatives

A separate model would require a correspondence argument.
Native execution would violate the shared resource policy.

## Consequences

The source stays read-only during verification.
Images require Kani 0.68, its compiler bundle, and [C Bounded Model Checker (CBMC) 6.11.0](https://github.com/model-checking/kani/blob/kani-0.68.0/kani-dependencies).
Reconsider bounds when an invariant needs longer sequences.

## Confirmation

Runner tests reject invalid plans and incomplete results.
All six harnesses and their cover goals pass within the declared limits.
