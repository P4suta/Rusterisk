---
date: 2026-11-24
status: accepted
---

# Compose signaling and call decisions in one owner task

## Context

The signaling and control tasks exchange effects and signals through bounded queues.
Each handover takes a round trip, and full queues suspend admission.
The decision revises [Serialize call decisions in a pure core](serialize-call-decisions-in-a-pure-core.md).

## Decision

One task owns the signaling engine and control decisions and applies each input's effects in one turn.
Both types take time as an input.
Choose one task when the median of five trial maxima is below the 20 ms budget.
Each trial reloads with 64 active calls.
During another reload, one decision ends all 64 calls between validation and publication.
In a two-processor container, the longest turn took 2.378 ms and the median of the five maxima was 1.674 ms.

## Alternatives

Two tasks with one-way messages shield signaling from a long decision and keep both queues.

## Consequences

A long control decision delays signaling in the same task.
The queues, the pending effects, and the profile capsules go away.
Reconsider when a measured turn reaches 20 ms.

## Confirmation

The fast gate runs one trial.
The task must pass the call compatibility tests.
