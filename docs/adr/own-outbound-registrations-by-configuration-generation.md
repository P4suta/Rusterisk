---
date: 2026-11-16
status: proposed
---

# Own outbound registrations by configuration generation

## Context

[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-10.2) defines registration refresh, authentication, and removal.
Registration settings lack a complete runtime driver.
[Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip_outbound_registration.c) defines compatibility defaults and retries.

## Decision

Serialize signaling decisions with pure registration transitions.
Key registrations by name and checked configuration generation.
Limit clients, shared setup slots, and retiring transactions to sixteen each.
Share sixty-four connection slots with calls.
Reuse bounded resolution, verified TLS, and connection generations.
Distinguish call and configuration lifetimes.
Reject stale completions before caching, connecting, or sending.
Keep one boot Call-ID and consecutive request sequences across replacements.
Serialize REGISTER transactions per registrar until final responses or timeouts.
Sixteen retirement slots cover one fully active generation.
Reject overflowing replacements before cancellation or publication, preserving current settings.
Matching final responses or original deadlines release retirement slots.
Refresh matching Contacts before expiry.
Apply response retry instructions and configured limits.
Dispatch requests round-robin.
Cancel previous generations before publication.
Preserve active calls.
Listener changes require restart.

## Alternatives

Detached loops duplicate limits and complicate replacement ordering.

## Consequences

Replacement acknowledgements must preserve event processing.

## Confirmation

Exercise authentication, refresh, rejection, retry exhaustion, disconnection, shutdown, and delayed completions across replacement generations with actual peers.
