---
date: 2026-11-20
status: proposed
---

# Separate terminal prefix from closure proofs

## Context

The terminal cleanup harness expanded independent four-operation prefixes and suffixes as one product.
The isolated harness needed 102 seconds, the complete campaign exhausted two GiB, and successful JSON occupied 5,356,510 bytes.
The terminal transition and post-terminal closure are independent properties.
Kani 0.68 evaluates proof definitions in reverse source order.

## Decision

Select one of two symbolic modes in the same harness.
The transition mode applies every four-operation prefix, then asserts complete cleanup after every terminal event.
The closure mode starts from a canonical ended call and applies every four-operation suffix.
Assert the same complete call value after each suffix operation.
Keep the existing limits, assertions, and cover goals without assumptions or stubs.
Define the terminal harness after every other harness.
Retained JSON is capped at eight MiB.

## Alternatives

Increasing memory would weaken the release profile.
Reducing either sequence would remove previously checked traces.

## Consequences

Kani preserves both complete sequence spaces without their unrelated Cartesian product.
The memory-intensive terminal harness runs before the smaller proofs.

## Confirmation

Both modes and the complete campaign of twenty-two harnesses must pass within the declared profile.
