---
date: 2026-11-16
status: proposed
---

# Own outbound registrations by configuration generation

## Context

[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-10.2) defines registration refresh, authentication, and removal.
Accepted registration settings have no running adapter.
[Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip_outbound_registration.c) supplies the compatibility defaults and retry rules.

## Decision

Keep registration transitions pure and serialize their decisions in signaling.
Key each registration by its name and checked configuration generation.
Own at most sixteen registration clients.
Share sixteen setup slots and sixty-four connection slots with calls.
Reuse the bounded resolver, verified TLS, and connection generations.
Distinguish call and configuration lifetimes explicitly.
Reject stale completions before caching, connecting, or sending.
Keep one boot Call-ID and increasing request sequence numbers across replacements.
Refresh the matching Contact before its granted expiry.
Apply finite transaction deadlines, response retry instructions, and configured retry limits.
Dispatch pending requests in round-robin order.
Cancel the previous generation before publishing replacement settings.
Preserve active calls and require restart for listener changes.

## Alternatives

Detached per-registration loops duplicate resource limits and complicate replacement ordering.

## Consequences

Registration settings and cancellation become observable behavior.
Runtime replacement needs an acknowledged signaling boundary.

## Confirmation

Exercise authentication, refresh, rejection, retry exhaustion, disconnection, shutdown, and delayed completions across replacement generations with actual peers.
