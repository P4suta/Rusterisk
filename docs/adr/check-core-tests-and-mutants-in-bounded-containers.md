---
date: 2026-11-16
status: proposed
---

# Check core tests and mutants in bounded containers

## Context

Core ownership checks require interpreted executions and tests that detect broken behavior.
[Miri](https://github.com/rust-lang/miri) interprets Rust tests, while [cargo-mutants](https://mutants.rs/list.html) inventories deliberate source changes.

## Decision

Bind an independent manifest to the production core and every integration test target.
Copy exact source into an owned workspace and normalize manifest paths.
Use Miri from `nightly-2026-08-21` and cargo-mutants 27.1.0.
Compare native discovery with the source, then require every test to pass without ignored or filtered tests.
Map a nonempty mutation inventory to ownership invariants.
Require a successful baseline and caught viable mutants for every invariant.
Keep missed mutations and timeouts incomplete.
Run one worker with two processors, 2 GiB of memory, 256 processes, and a fifteen-minute limit per phase.

## Alternatives

The whole workspace requires unrelated dependency metadata.
Native execution conflicts with the shared resource policy.

## Consequences

Snapshots require source equality checks and cleanup.
Miri setup may fetch dependencies of the standard library before isolated execution.
Reconsider the scope when core dependencies or test targets change.

## Confirmation

Reject empty inventories, skipped tests, incomplete outcomes, and cleanup failures.
Run maintained commands on the actual source.
