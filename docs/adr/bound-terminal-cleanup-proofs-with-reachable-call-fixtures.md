---
date: 2026-11-20
status: proposed
---

# Bound terminal cleanup proofs with reachable call fixtures

## Context

The terminal cleanup harness expanded two independent four-operation arrays over nineteen operations.
The isolated harness needed 102 seconds, and the complete two-GiB campaign exhausted memory after the other proofs.
The contract concerns reachable pre-terminal resources and every operation after termination, rather than every redundant spelling of the same state.

## Decision

Construct a finite set of reachable call fixtures through production operations.
Include down, ringing, early media, answered, fully owned, and already ended calls.
Apply every symbolic terminal event, assert complete cleanup, then apply one arbitrary operation and assert absorption.
Keep the existing resource limits, assertions, and cover goals without assumptions, stubs, or experimental contracts.

## Alternatives

Increasing memory would weaken the release profile without improving the property.
Keeping repeated post-terminal operations proves the same absorbing transition through a larger product.

## Consequences

Kani explores each operation and each representative reachable resource state without the redundant sequence product.
Unit and integration tests continue to cover longer operation sequences.

## Confirmation

The isolated harness and the complete eighteen-harness campaign must pass within the declared profile.
