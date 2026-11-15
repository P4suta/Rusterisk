---
date: 2026-11-15
status: proposed
---

# Release wildcard interests through public identifiers

## Context

The public subscription contract accepts `bridge:` for all bridge events.
Asterisk 22.9.0 accepts that subscription but returns `501` when the same URI requests removal.
Its [unsubscribe handler](https://github.com/asterisk/asterisk/blob/22.9.0/res/stasis/app.c) treats a null identifier as the wildcard but passes an empty identifier through unchanged.
The [request adapter](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_stasis.c) supplies the empty string.

## Decision

Release wildcard interests through the same public URI used to create them.
Compare the deliberate successful response with the reference failure explicitly.
Keep wildcard snapshot identifiers compatible.

## Alternatives

Reproducing the reference failure preserves its error but prevents selective cleanup.
Using private topic identifiers exposes implementation details to clients.

## Consequences

Clients can release wildcard interests without disconnecting their app.
No Rusterisk client migration is necessary.
Reference clients can disconnect their app to clear its interests.
Reconsider the difference when the reference fixes wildcard removal.

## Confirmation

Check acquisition, idempotence, snapshot identifiers, removal, and app cleanup.
Keep the record proposed until reference call comparisons pass.
