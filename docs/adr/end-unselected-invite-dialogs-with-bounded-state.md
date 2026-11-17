---
date: 2026-11-17
status: proposed
---

# End unselected `INVITE` dialogs with bounded state

## Context

[RFC 3261 §13.2.2.4](https://www.rfc-editor.org/rfc/rfc3261.html#section-13.2.2.4) requires `ACK` for each 2xx and `BYE` for unwanted dialogs.
The [target decision](validate-dialog-targets-before-state-changes.md) excludes forks.

## Decision

Preserve the selected dialog and sequence.
Validate targets under [RFC 3261 §12.1.2](https://www.rfc-editor.org/rfc/rfc3261.html#section-12.1.2) and [`SIPS` requirements](https://www.rfc-editor.org/rfc/rfc5630.html#section-5.1.2).
Keep each fork's first `ACK` before `BYE` admission.
Use tag-specific branches and one Engine table for 128 forks across 64 outgoing calls.
Limit metadata to 16 KiB and summed `Vec::capacity()` to 2 MiB.
Each `ACK` remains limited to 16 KiB.
Find retained packets before interpreting another Contact.
Use closed `Pending`/`Sent` states.
Only successful transaction admission marks `Sent`.
Release entries and byte charges at the original 32-second deadline.
Preserve peer, credentials, and URI security.

## Alternatives

Marker-only reconstruction can redirect `ACK` after Contact changes.
Separate packet limits per call introduce more refusals.
Unbounded retention violates resource limits.

## Consequences

Saturation returns `ACK` and typed teardown failure, preserving the selected call.
Retention costs up to 2 MiB of buffer capacity plus 16 KiB of metadata per Engine.
Unusable targets leave mandatory-`ACK` coverage unresolved.

## Confirmation

Test replay, teardown, branches, expiry, saturation, security, and credentials.
Check memory layout and capacity accounting.
Reconsider bounds when supported workloads require more forks.
