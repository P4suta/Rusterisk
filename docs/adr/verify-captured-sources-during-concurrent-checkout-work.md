---
date: 2026-11-16
status: proposed
---

# Verify captured sources during concurrent checkout work

## Context

The fuzz runner builds an owned source copy through production path dependencies.
Comparing the live checkout after execution also rejects unrelated concurrent edits.
The workers mount the captured sources read-only.

## Decision

Bind compilation and validation to the captured production files.
Compare the captured source tree with its original in-memory bytes after execution.
Keep manifest, dependency lock, harness, resource, and native completion checks.
Report a captured candidate result without claiming that later checkout edits passed.
Run applicable checks again when their production inputs change.

## Alternatives

Freezing the entire checkout prevents independent implementation work.
Skipping source comparison would leave captured-file changes unchecked.

## Consequences

Independent checkout edits can continue during bounded native execution.
Captured source changes still reject the run.

## Confirmation

Require fixtures that distinguish checkout changes from captured-file changes.
Complete the native campaign before claiming successful fuzz execution.
