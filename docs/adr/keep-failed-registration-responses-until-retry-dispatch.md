---
date: 2026-11-16
status: proposed
---

# Keep failed registration responses until retry dispatch

## Context

Registrations share checked CSeq state while separate addresses of record can progress during retry waits.
Authentication consumes responses, including rejected attempts.
The [reference driver](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip_outbound_registration.c#L1557-L1567) keeps the transmitted request for later attempts.

## Decision

Keep an owned failed response in the waiting state.
Return either an authenticated transaction or the rejected response through a closed result type.
Preserve the existing authentication API through a wrapper.
Construct retry requests only when due.
Advance CSeq at dispatch.
Renew authentication scopes using the original Request-URI octets.
Recheck connection generation before sending.
Cancellation drops waiting work without closing shared flows.

## Alternatives

Preparing requests before waiting reserves CSeq values ahead of other senders.
Borrowing in the public API avoids copies but permits repeated authentication transitions.
Cloning responses preserves the existing return shape but copies complete messages.

## Consequences

The public library gains an owned authentication result.
Waiting data remains bounded by the existing client and message limits.

## Confirmation

Pure tests must preserve rejected responses, authentication counters, and wire octets.
Native tests must cover another address of record progressing during a wait, dispatch order, cancellation, and stale connections.
