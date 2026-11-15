---
date: 2026-11-15
status: proposed
---

# Own signaling streams by connection generation

## Context

[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-18.2.2) binds reliable responses to the connection that received the request.
The existing signaling loop serializes engine decisions and media control.

## Decision

Keep listener engines in the signaling loop.
Give accepted and connected streams monotonic connection generations.
Move concrete streams into owned generic tasks with partial input and bounded output.
Order messages and completion in one bounded queue and drain responses before closing.
Run connection attempts and encrypted handshakes as owned jobs with deadlines.
Reuse authenticated registration flows and release their contacts, transactions, and dialogs when the generation closes.
Bound active streams, pending jobs, queued packets, frame assembly, and shutdown.
Preserve reliable transaction deadlines without datagram retransmission timers.

## Alternatives

One independent engine per socket would split endpoint registration and dialog routing.
Shared sockets would require mutable coordination between readers and writers.
Blocking connection setup would stall unrelated signaling.

## Consequences

Stream failures end only their bound flows.
Reconsider the boundary if routing requires independent transport ownership.

## Confirmation

Native TCP and TLS tests must cover registration, call control, partial input, duplicate messages, generation replacement, disconnect, saturation, and shutdown.
Initial encrypted outbound connections must verify trust roots and peer names.
