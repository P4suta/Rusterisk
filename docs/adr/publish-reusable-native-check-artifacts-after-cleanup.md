---
date: 2026-11-18
status: proposed
---

# Publish reusable native check artifacts after cleanup

## Context

[Required outcomes](require-runner-outcomes-for-candidate-verification.md) span native hosts.
Existing diagnostic trees survive failed execution and cleanup.

## Decision

Trust unchanged maintained producers and their storage and transfer custody.
Keep native outputs, captured inputs, actual tool versions, and bounded original execution paths.
Compare recorded paths using the producer platform's syntax and keep them separate from local file access.
Publish reusable artifacts through a [same-filesystem rename](https://doc.rust-lang.org/std/fs/fn.rename.html) only after validation, source rechecks, supervisor completion, and owned cleanup succeed.
Never import existing diagnostic trees retrospectively.
Reconstruct outcomes through maintained readers, including the [native metadata validator](../../xtask/src/distribution/sdk_consumer_metadata.rs), with exact source, target, profile, tool, applicable image, and inventory checks.
Create no manual receipts, JSON dossiers, or pass flags.

## Alternatives

Live native reruns avoid historical custody at greater execution and connectivity cost.
Console logs can't establish successful cleanup.

## Consequences

Retained inputs require storage.
Hashes detect mismatches but can't establish historical execution or exclude malicious regeneration.
Reconsider reuse if producer or custody trust changes.

## Confirmation

Start with macOS ARM64.
Reject incomplete, mismatched, or failed artifacts.
Verify successful publication and cleanup-failure rejection on macOS and Linux.
Verify Windows drive paths with and without the [extended-length prefix](https://doc.rust-lang.org/std/fs/fn.canonicalize.html#platform-specific-behavior).
