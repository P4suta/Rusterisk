---
date: 2026-11-17
status: proposed
---

# Reuse live signaling flows after earlier candidates fail

## Context

Initial setup checks only its first compatible candidate for reuse.
[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-18.1.1) recommends matching connection reuse.
[RFC 2782](https://www.rfc-editor.org/rfc/rfc2782.html) requires preserving candidate priority.

## Decision

Before opening each ordered candidate, the existing owned setup job requests admission from the Network actor through a bounded channel.
Return reuse, connection permission, or rejection through a closed enum.
Validate the current owner, setup key, open connection generation, original domain, and captured TLS profile before publishing reuse.
Keep the original eight-second deadline, sixteen setup slots, and sixty-four connection slots.
Cancellation interrupts admission waits.
Reuse leaves admitted flows under Network ownership.

## Alternatives

Fresh sockets after the first failure simplify control flow but add connections.
Selecting an existing backup first bypasses candidate priority.
Copying available flows into workers risks stale admission.

## Consequences

Each attempted candidate adds an actor exchange.
Reconsider the exchange if measured admission delays prevent bounded setup completion.
Persistent resolution cursors, shared 503 suppression, and configuration replacement remain separate decisions.

## Confirmation

Native TCP and TLS tests must cover preferred failure followed by backup reuse, preferred success, cancellation, closed flows, mismatched profiles, stale generations, and shared capacity.
