---
date: 2026-11-15
status: proposed
---

# Separate dialog identity from protocol call identifiers

## Context

[Session Initiation Protocol (SIP) dialog identification](https://datatracker.ietf.org/doc/html/rfc3261#section-12) uses Call-ID and both tags.
Indexing incoming work only by Call-ID lets another initial request replace an existing invitation.

## Decision

Index owned incoming invitations and dialogs by their unique internal call identifiers.
Match received dialog requests with Call-ID, local tag, remote tag, and the established peer.
Match cancellation with the original invitation transaction.
Detect merged requests before creating another call, following [RFC 3261 section 8.2.2.2](https://datatracker.ietf.org/doc/html/rfc3261#section-8.2.2.2).
Reject unknown tagged dialogs without creating a call.
Keep unsupported dialog updates separate from initial invitation admission.

## Alternatives

Rejecting every reused Call-ID blocks independent dialogs with different tags.
Keeping Call-ID indexes with collision checks still conflates protocol identity and task ownership.

## Consequences

Incoming request lookup scans at most 64 owned records.
Existing calls survive rejected or unrelated requests.
Reconsider lookup structure if measured routing latency exceeds its budget.

## Confirmation

Test reused Call-ID values, merged branches, dialog tags, cancellation, retransmission, and independent cleanup.
Keep the record proposed until reference and transport checks pass.
