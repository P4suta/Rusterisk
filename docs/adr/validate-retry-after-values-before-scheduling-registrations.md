---
date: 2026-11-16
status: proposed
---

# Validate Retry-After values before scheduling registrations

## Context

[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-20.33) permits comments and parameters in Retry-After.
The [pinned parser](https://github.com/restsend/rsipstack/blob/ca99be1e736f8f6fed3e2f1e48de12384dac9733/src/sip/parser.rs#L265-L270) validates the numeric prefix without checking the remaining syntax.
The [reference driver](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip_outbound_registration.c#L1465-L1478) prioritizes positive Retry-After values over configured retry limits.

## Decision

Validate complete unfolded values before scheduling.
Preserve [permitted whitespace normalization](https://www.rfc-editor.org/rfc/rfc3261.html#section-25.1).
Accept nested comments, quoted escapes, duration, and generic parameters.
Require unsigned decimal seconds within `u32`.
Represent zero or an absent header as no positive instruction.
Reject repeated headers, repeated parameter names, malformed syntax, and overflow.
Bound input by the existing message limit and parameter names by sixteen entries.
Use iteration without recursive comment parsing.
Keep parsing separate from retry counters and transport selection.

## Alternatives

Reading only the prefix tolerates malformed peers but leaves suffix syntax unchecked.
Another parser dependency could provide the grammar but adds maintenance for one header field.

## Consequences

Reconsider the parameter limit if supported peers need more entries.
Scheduling, alternate servers, and shared server suppression remain incomplete.

## Confirmation

Pure wire tests must cover valid syntax, folding, nesting, duplicate names, integer boundaries, parameter capacity, and rejected trailing bytes.
