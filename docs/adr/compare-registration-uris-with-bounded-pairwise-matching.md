---
date: 2026-11-16
status: proposed
---

# Compare registration URIs with bounded pairwise matching

## Context

[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-19.1.4) defines nontransitive comparison for Session Initiation Protocol (SIP) URIs.
Distinct extension values can produce two bindings that both match a third URI.

## Decision

Keep original URIs and parsed routing metadata.
Compare candidates pairwise before atomic mutation.
Reject candidates that match more than one binding or duplicate another candidate.
Preserve userinfo case, method case, explicit ports, and reserved escapes.
Complete header-specific matching rules before declaring URI comparison complete.

## Alternatives

Selecting the first match depends on storage order.
Updating every match changes more than one stored binding.
A normalized key can't represent nontransitive comparison.

## Consequences

Contact limits bound pairwise work.
Ambiguous extensions require an explicit URI.
Header semantics and reference behavior remain completion conditions.

## Confirmation

Test symmetry, escapes, case, ports, parameter order, header multiplicity, and the nontransitive counterexample.
Reconsider rejection if reference evidence establishes a safe interoperable rule.
