---
date: 2026-11-16
status: proposed
---

# Require runner outcomes for candidate verification

## Context

The approved plan requires applicable checks and startup from distributed bytes.
Existing [candidate readers](verify-distributions-from-local-candidate-bytes.md) bind SDK and server inputs differently.

## Decision

Use a closed required inventory and one captured source set.
Derive each runner's input projection without comparing unrelated digest algorithms.
Require fast, security, reference, browser, core, fuzz, native, SDK, five target, and two Linux OCI checks.
Construct typed outcomes only after native validation and owned cleanup.
Select heavy actions explicitly and run one bounded job at a time.
Treat dry runs as plans and reject missing validation.
Reuse unchanged source, profile, tool, image, and inventory inputs through maintained native validators.
Keep existing native artifact formats and captured inputs when reuse requires retention.
Discard build state and keep reusable artifacts after cleanup.
Create no manual receipts, evidence JSON, or passed flags.
Keep publication and signing separate.

## Alternatives

Always rerunning supplies fresh results at greater cost.
Console-log matching can't reconstruct native success.

## Consequences

Retaining Kani output changes the [temporary-result policy](verify-call-ownership-in-bounded-kani-containers.md).

## Confirmation

Reject missing, duplicate, stale, incomplete, and mismatched inputs or outcomes.
Keep proposed until every required runner connects and passes.
