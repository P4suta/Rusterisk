---
date: 2026-11-17
status: proposed
---

# Keep signaling flow identity through transaction settlement

## Context

[RFC 3261 section 18.2.2](https://www.rfc-editor.org/rfc/rfc3261.html#section-18.2.2) requires reliable responses on the original open connection.
Reload can require different trust contexts for the same listener and peer.
Peer lookup alone loses connection identity.

## Decision

Give each stream a checked, non-reused flow identifier.
Keep transport provenance in canonical signaling owners, including dialogs, transactions, cached replies, and late fork cleanup.
Preserve independent [transfer provenance](bind-transfer-reply-routes-to-server-transactions.md).
Carry exact flow identity through receive, send, refusal, and disconnection.
Own at most sixty-four streams and a bounded reusable-flow lookup.
Send scoped packets directly through their captured flow.
Never redirect a scoped packet through peer lookup.
Keep existing single-flow APIs separate from scoped entrypoints.

## Alternatives

Active-call routes miss replies after call release.
Independent engines split registration and dialog routing.
Keeping one connection prevents trust-context coexistence.

## Consequences

Signaling owners need transport metadata.
Closed captured flows fail to prevent reuse across trust contexts.
The project adopts a stricter limit than the RFC's reconnection recommendation.
Reconsider when reconnecting established transactions becomes required.

## Confirmation

Use actual coexisting encrypted streams to check received packets, cached replies, late answers, cancellation, disconnection, saturation, and shutdown.
Keep proposed pending native verification.
