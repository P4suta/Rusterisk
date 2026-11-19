---
date: 2026-11-20
status: proposed
---

# Separate finite capacity tests from symbolic settings proofs

## Context

The [container scratch decision](use-container-owned-disk-scratch-for-kani.md) requires reconsideration when unchanged harnesses exceed its limits.
The saturated harness timed out under two solvers and at 600 seconds.
The complete campaign then timed out three ownership harnesses during repeated 65-slot absence scans.
Finite construction and scans differ from symbolic transition boundaries.

## Decision

Exercise 65 generations, every slot, and absent generations in unit tests through production operations.
Reject a foreign reference by its domain identifier before searching local slots.
Compile symbolic settings proofs with the two slots required by their current and retired generations.
Use Kani for arbitrary owners, reference counts, and transition results through the same operations.
Keep eighteen harnesses, exact result validation, Kissat 4.0.1, container scratch, and existing limits.

## Alternatives

Longer timeouts and CaDiCaL did not complete the saturated harness.
The unstable [Kani contract interface](https://model-checking.github.io/kani/crates/doc/kani/contracts/index.html) would add assumptions.

## Consequences

Unit tests establish finite reachability while Kani establishes symbolic boundaries.
Reconsider when capacity becomes dynamic or stable contracts remove the scan product.

## Confirmation

Core tests must compare the modeled state and reuse all 64 retired slots.
The Kani result must contain eighteen exact successes.
