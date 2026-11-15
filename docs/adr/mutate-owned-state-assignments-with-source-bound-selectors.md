---
date: 2026-11-16
status: proposed
---

# Mutate owned state assignments with source-bound selectors

## Context

The pinned [mutation generator](https://github.com/sourcefrog/cargo-mutants/blob/v27.1.0/src/visit.rs) replaces call-returning functions with default values.
The core defines no default call or decision.
Its terminal and connection-claim candidates fail compilation.

## Decision

Keep the complete generated campaign and its failure gates.
Supplement two actual state assignments with bounded source mutations.
Parse the production source and require exactly one matching method, assignment, and literal source fragment.
Change the terminal state to active and the claimed connection to available in separate owned snapshots.
Build each mutation successfully before requiring the actual core tests to reject its behavior.
Keep production files unchanged and verify snapshot identity before cleanup.

## Alternatives

Accepting only build failures leaves behavior unchecked.
Adding default domain values solely for mutation would weaken the core contract.
Copied models could diverge from production.

## Consequences

Review the bounded mutations when source changes invalidate selectors.
Native build failure, missing tests, timeout, or a surviving mutation rejects the campaign.

## Confirmation

Test missing, duplicate, moved, and changed assignments.
Run both source mutations and every generated candidate with bounded resources.
