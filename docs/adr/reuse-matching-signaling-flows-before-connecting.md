---
date: 2026-11-16
status: proposed
---

# Reuse matching signaling flows before connecting

## Context

[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-18.1.1) recommends reusing matching connections.
Connection reservations begin before resolution, blocking reuse when sixty-four streams are open.

## Decision

Use independent limits of sixteen setup metadata slots and sixty-four connection slots.
Preserve [candidate priority](https://www.rfc-editor.org/rfc/rfc2782.html).
Capture configuration generation and the immutable TLS context at setup admission.
Compare the starting TLS profile, verified original domain, listener, and connection generation before reuse.
Reject cancelled requests, closed streams, unverified TLS peers, and stale profiles before publishing reuse.
Return `Reused(ConnectionKey)` or `Connecting` through a closed enum.
Release reuse metadata after its resolution job joins.
Network ownership keeps admitted flows alive independently of requesting leases.
Cancelling a reuse request never closes an admitted flow.

## Alternatives

Reserving capacity before resolution is simpler but blocks reuse at capacity.
Connecting before checking reuse creates avoidable sockets and handshakes.

## Consequences

Borrowed foreign configuration identity distinguishes immutable TLS profiles without adding shared domain state.
Accepted TLS connections don't establish registrar identity.

## Confirmation

Native registrations must share established flows without another connection attempt.
Rejecting tests must cover changed trust roots, client identity, configuration generation, cancellation, closed streams, stale generations, and capacity.
