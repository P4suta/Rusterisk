---
date: 2026-11-15
status: proposed
---

# Match server transactions by Via identity

## Context

[Server transaction matching](https://datatracker.ietf.org/doc/html/rfc3261#section-17.2.3) uses branch, sent-by, and method for requests with the standard branch cookie.
Raw header equality misclassifies retransmissions when parameter order changes.

## Decision

Use a typed transaction key containing the established peer, branch, parsed sent-by, and method token.
Normalize domain names while preserving branch and method case.
Replay the stored response before authentication or call admission.
Preserve strict request matching for the existing legacy fallback.
Keep dialog identity and transaction identity distinct.

## Alternatives

Sorting an entire Via header still includes parameters outside the transaction identity.
Using branch alone permits collisions across clients and methods.
Removing peer scope broadens the current transport trust boundary.

## Consequences

Equivalent retransmissions share bounded cache entries and emit no repeated call events.
Reconsider peer scope when implementing authenticated transport migration.

## Confirmation

Verify parameter order, case, distinct sent-by values, method separation, final responses, and unchanged independent dialog cleanup.
Keep the record proposed until reference checks pass.
