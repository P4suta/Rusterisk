---
date: 2026-11-15
status: proposed
---

# Derive response tags without transaction state

## Context

[Stateless responses](https://datatracker.ietf.org/doc/html/rfc3261#section-8.2.7) must regenerate the same tag for an identical request.
Fresh randomness makes repeated rejected requests produce different tags.
Caching invalid dialog requests can reserve a transaction identity before a valid request arrives.

## Decision

Derive missing response tags from the request and private engine seed using RustCrypto `Hmac<Sha256>` from `hmac` 0.13.0.
Separate response-tag derivation from other token purposes.
Keep established dialog tags and the tag omission for 100 responses.
Require prepared tags at the shared response formatter.
Keep rejected dialog lookups free of transaction state.

## Alternatives

Caching every rejection consumes admission capacity and permits invalid requests to block valid ones.
An unkeyed request hash makes tags predictable.

## Consequences

Identical requests in one engine reproduce their response tags.
Changing the private seed changes derived tags.
The dependency includes `ctutils` and architecture-specific `cmov` arithmetic.
Reconsider only if protocol or measured rendering costs require another construction.

## Confirmation

Test repeatability, seed separation, distinct requests, supplied tags, provisional responses, and rejected-request isolation.
Keep the record proposed until reference checks pass.
