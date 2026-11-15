---
date: 2026-11-16
status: proposed
---

# Apply registration bindings by address of record

## Context

[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-10.3) requires atomic registration updates and complete binding responses.
[Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip_registrar.c) resolves authorized addresses through the endpoint's configured addresses of record.

## Decision

Resolve authenticated REGISTER targets through endpoint addresses and the To URI.
Keep authentication credentials independent of the selected address.
Reject registration when the configured contact limit is zero.
Keep at most sixteen contacts per address and 128 dynamic bindings per signaling engine.
Bound header entry framing before parsing URI values.
Keep URI parameters and a distinct physical routing peer.
Validate changes before replacing the binding set.
Use each Contact's `expires` before the `Expires` header.
Support binding queries, targeted removal, and wildcard removal.
Return the complete remaining binding set.
Reject stale request sequences without changing bindings.
Select live contacts deterministically using their preferences.
Publish endpoint availability when its first binding appears or last disappears.

## Alternatives

Keeping the first Contact loses bindings.
Incremental mutation leaves partial updates after validation failures.

## Consequences

Binding changes preserve active dialogs.
Fixtures must name permitted registration addresses.

## Confirmation

Compare repeated headers, lists, expiry, capacity, queries, removals, stale sequences, and routing with the reference.
