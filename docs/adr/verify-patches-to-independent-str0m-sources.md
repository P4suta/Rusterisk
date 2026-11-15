---
date: 2026-11-16
status: proposed
---

# Verify patches to independent str0m sources

## Context

str0m 0.25.0 [selects sending queues by media and restriction identifiers](https://github.com/algesten/str0m/blob/5f2183ce7c67117cd1960dfeb11bed11fc188b66/src/session.rs#L1236), so distinct synchronization sources collide.
Dynamic receiving source replacement discards existing reception history.
[Different clocks require distinct sources](https://www.rfc-editor.org/rfc/rfc7160#section-4.1) with control reports.

## Decision

Vendor the published package through a Cargo path patch.
Preserve version 0.25.0, licenses, and the published package checksum.
Add source-specific sending queues and optional bounded receiving contexts.
Reject failed authentication without replacing established contexts.
Keep authentication, replay protection, rollover, and control reports per source.
Verify every published file hash after reversing the recorded patch.
Capture the dependency and provenance in verification snapshots.

## Alternatives

The published implementation stalls a second sending source.
Negotiated restriction identifiers require peer agreement.
A shared source with different clocks violates the selected reporting rules.
An unpinned upstream revision changes the approved dependency contract.

## Consequences

Maintain and review the local patch.
Reconsider vendoring when a published release passes the same source tests.

## Confirmation

Separate encrypted sending and receiving tests must preserve authentication, replay protection, rollover, existing replacement behavior, and secure duplex audio with distinct clocks.
Require formatting, source integrity, dependency, license, Rust 1.89, and browser checks.
