---
date: 2026-11-24
status: accepted
---

# Compose signaling and call decisions in one owner task

## Context

The signaling task and the control task exchange effects and signals through bounded queues.
A handover needs a round trip, and a full queue suspends admission.
This replaces the two tasks of [Serialize call decisions in a pure core](serialize-call-decisions-in-a-pure-core.md) and sets its open budget.

## Decision

Let one task own the signaling engine and the control decisions, and apply every effect of an input in its turn.
Both stay separate types that take time as a value.
The integration budget for one turn is 20 ms.
Each trial reloads the configuration with 64 active calls before one shutdown decision ends them.
In a two-processor container, the longest turn was 1.249 ms and the median of five trial maxima was 0.851 ms.

## Alternatives

Two tasks with one-way messages shield signaling from a long decision and keep both queues.

## Consequences

A long control decision delays signaling in the same task.
The queues, the pending effects, and the profile capsules go away.
Reconsider when a measured turn reaches 20 ms.

## Confirmation

The fast gate must run one trial of the measurement.
The composed task must pass the call compatibility tests.
