---
date: 2026-11-16
status: proposed
---

# Retry registrations with validated Min-Expires values

## Context

[RFC 3261 §10.2.8](https://www.rfc-editor.org/rfc/rfc3261.html#section-10.2.8) permits registration retries after 423 responses.
The [registration client](https://github.com/pjsip/pjproject/blob/2.16/pjsip/src/pjsip-ua/sip_reg.c#L1171-L1235) bundled with [Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/third-party/versions.mak) raises `Expires` before retrying.

## Decision

Require one positive decimal Min-Expires greater than the transmitted `Expires`.
Never turn removal into registration.
Preserve the transmitted URI, Contact, Call-ID, From, To, transport, authentication scopes, and authentication attempt budget.
Advance CSeq, branch, and each Digest nonce count before retransmitting.
Keep the increased `Expires` for later refreshes.
Keep response correction separate from delayed failure scheduling.

## Alternatives

The reference client guesses 3601 seconds once when Min-Expires is absent.
Reject malformed responses instead because [RFC 3261 §10.3](https://www.rfc-editor.org/rfc/rfc3261.html#section-10.3) requires the registrar to supply Min-Expires.
Ignoring 423 avoids retry machinery but prevents valid registrations accepted by the reference implementation.

## Consequences

Malformed reference peers can behave differently.
Reconsider guessing when a supported peer needs the behavior.
Manager integration remains incomplete.

## Confirmation

Pure and native tests must cover valid corrections, repeated corrections, removal, malformed values, identity limits, authenticated requests, refreshes, and cancellation.
