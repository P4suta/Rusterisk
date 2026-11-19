---
date: 2026-11-20
status: proposed
---

# Separate finite capacity tests from symbolic settings proofs

## Context

The [container scratch decision](use-container-owned-disk-scratch-for-kani.md) requires reconsideration when unchanged harnesses exceed its limits.
With two CPUs and 2 GiB, the saturated settings harness timed out at 120, 300, and 600 seconds under Kissat and at 120 seconds under CaDiCaL.
The 65-generation path is finite, while other settings harnesses cover symbolic ownership.

## Decision

Test the 65-generation path with production operations, exact model comparison, and every releasable slot.
Replace its Kani entry with a symbolic reference-count boundary over every `u64` count and arbitrary owner.
Keep eighteen harnesses, exact result validation, Kissat 4.0.1, container scratch, and existing limits.
Only the earlier unchanged-harness condition changes.

## Alternatives

Increasing the timeout failed at 600 seconds.
Changing the SAT solver failed at 120 seconds.
The [Kani contract interface](https://model-checking.github.io/kani/crates/doc/kani/contracts/index.html) remains unstable and would add assumptions.

## Consequences

Kani verifies the symbolic reference-count transition instead of replaying deterministic construction.
Reconsider when the full proof completes under these limits without experimental contracts, or when capacity becomes dynamic.

## Confirmation

Core tests must reach 65 generations, compare every modeled slot and handle, and reuse all 64 releasable slots.
The Kani result must contain eighteen exact successes, including the symbolic boundary harness.
