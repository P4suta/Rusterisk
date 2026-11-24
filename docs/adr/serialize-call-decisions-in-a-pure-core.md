---
date: 2026-11-14
status: accepted
---

# Serialize call decisions in a pure core

## Context

Call cancellation, late answers, and bridge membership must agree across signaling and media tasks.
The approved plan requires a dependency-free Rust core and bounded communication.

## Decision

Keep call transitions and resource ownership in a `no_std` crate with fixed capacities.
One owner task applies decisions, as [Compose signaling and call decisions in one owner task](compose-signaling-and-call-decisions-in-one-owner-task.md) records.
Event ingestion continues while clients await call milestones.

## Alternatives

Shared mutable call objects would simplify callbacks but require locks across competing transitions.
A separate process per call would isolate failures but increase transport and recovery work.

## Consequences

Adapters perform network and storage effects after the core validates transitions.
Capacity exhaustion is an explicit error.
The AudioSocket enum keeps its bounded audio frame inline and records one `large_enum_variant` expectation.
Heap indirection would add an allocation to every audio message and conflict with the ownership constraint.

## Confirmation

The approved plan accepts the ownership boundary.
Transition tests cover cancellation, duplicate events, terminal states, and cleanup.
Release proofs check bounded transition sequences in the production core.
