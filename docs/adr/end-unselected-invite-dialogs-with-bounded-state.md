---
date: 2026-11-17
status: proposed
---

# End unselected `INVITE` dialogs with bounded state

## Context

[RFC 3261 §13.2.2.4](https://www.rfc-editor.org/rfc/rfc3261.html#section-13.2.2.4) requires `ACK` for each 2xx and `BYE` for unwanted dialogs.
The [single-dialog target decision](validate-dialog-targets-before-state-changes.md) leaves forks unresolved.

## Decision

Preserve the selected dialog and sequence.
Validate targets under [RFC 3261 §12.1.2](https://www.rfc-editor.org/rfc/rfc3261.html#section-12.1.2) and [`SIPS` requirements](https://www.rfc-editor.org/rfc/rfc5630.html#section-5.1.2).
Reconstruct `ACK` from the unchanged `INVITE` and matched 2xx.
Use tag-specific branches and one Engine table with 128 markers for admitted `BYE` transactions.
Keep total table storage within eight kibibytes for 64 outgoing calls.
End retention at the original 32-second deadline.
Preserve peer, credentials, and URI security.
Reserve existing `BYE` transaction capacity before committing a marker.

## Alternatives

Eight cached `ACK` packets simplify replay but add an `ACK` limit.
Stateless cleanup can repeat `BYE` after settlement.
Unbounded retention violates resource limits.

## Consequences

Saturation returns `ACK` and typed teardown failure without ending the selected call.
Report unusable targets as typed failure without substituting the selected target.
Failing closed doesn't establish complete mandatory-`ACK` coverage.

## Confirmation

Test selection, cancellation, branches, retransmission, expiry, capacity, secure targets, and credentials.
Reconsider bounds when supported workloads require more forks.
