---
date: 2026-11-20
status: proposed
---

# Separate terminal prefix from closure proofs

## Context

The terminal cleanup harness expanded independent four-operation prefixes and suffixes as one product.
The isolated harness needed 102 seconds, and the complete two-GiB campaign exhausted memory.
The terminal transition and post-terminal closure are independent properties.

## Decision

Select one of two symbolic modes in the same harness.
The transition mode applies every four-operation prefix, then asserts complete cleanup after every terminal event.
The closure mode starts from a canonical ended call and applies every four-operation suffix.
Assert the same complete call value after each suffix operation.
Keep the existing limits, assertions, and cover goals without assumptions or stubs.

## Alternatives

Increasing memory would weaken the release profile.
Reducing either sequence would remove previously checked traces.

## Consequences

Kani preserves both complete sequence spaces without their unrelated Cartesian product.

## Confirmation

Both modes and the complete eighteen-harness campaign must pass within the declared profile.
