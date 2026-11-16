---
date: 2026-11-16
status: proposed
---

# Bind transfer reply routes to server transactions

## Context

Cached `REFER` replies can outlive notification usage.
[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-17.2.3) requires transaction context because legacy response headers lack Request-URI.

## Decision

Tag packets with their originating `ReferKey` inside `Engine`.
Preserve provenance in reply caches and notification retries.
Capture network routes before Controller mutation.
Use the tag and existing transaction expiry to keep routes after subscription termination.
Check peer and connection generation on every send.
Prune expired routes before admission.
Bound each engine to 64 notification owners plus 256 server transactions.
Reject capacity before Controller mutation.
Keep call, registration, and target lifetimes independent.

## Alternatives

Parsing headers uses fewer fields but can't recover legacy Request-URI.
An independent retired map duplicates expiry and admission checks.

## Consequences

Packet provenance adds exhaustive dispatch.
Reconsider changes to provenance, cache lifetime, or the 64/256 limits.

## Confirmation

Replay cached `202` after final `NOTIFY` settlement.
Deliver through original connections and suppress replacements.
Exercise mismatched transactions, expiry, saturation, disconnect, and shutdown without stopping unrelated calls, registrations, or target audio.
