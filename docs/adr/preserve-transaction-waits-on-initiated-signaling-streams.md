---
date: 2026-11-16
status: proposed
---

# Preserve transaction waits on initiated signaling streams

## Context

[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-18) recommends retaining reliable connections through transaction completion.
[Timer F](https://www.rfc-editor.org/rfc/rfc3261.html#section-17.1.2.2) normally permits 64*T1.
A five-second first-message timer also applies while initiated connections await responses.

## Decision

Keep accepted connections' five-second first-message deadline.
Let transaction owners govern response waits on initiated connections.
Start an absolute five-second assembly deadline when an idle stream receives bytes.
Keep partial-message deadlines absolute for both roles.
Prevent keepalives from extending accepted admission or partial-message deadlines.
Keep cancellation by connection generation, bounded queues, sixty-four connection slots, and joined shutdown.

## Alternatives

A universal five-second timer shortens valid transactions.
A universal longer timer weakens accepted-connection admission.
Sliding deadlines permit indefinite partial frames.

## Consequences

Idle initiated streams follow the existing admitted-flow lifetime.
Transaction timeout doesn't itself release a shared connection slot.

## Confirmation

Actual TCP tests must cover delayed registration responses, accepted silence, non-extending keepalives, partial-frame expiry, generation isolation, cancellation, and shutdown.
