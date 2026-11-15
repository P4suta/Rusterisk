---
date: 2026-11-15
status: proposed
---

# Intersect peer direction with local audio controls

## Context

[Offer/answer media directions](https://datatracker.ietf.org/doc/html/rfc3264#section-6.1) constrain sending and receiving independently.
Local audio controls must survive a peer's hold or resume request.

## Decision

Represent negotiated peer direction and local audio controls as distinct core state.
Intersect their permissions when computing the effective audio gate.
Carry direction changes through owned transport controls with the current call generation.
Suppress transport sending or delivery when negotiation forbids it.
Accept direction updates that preserve the selected codec and transport.
Reject unsupported offers without changing the active session.
Keep request acceptance, media configuration, and received audio as separate observations.

## Alternatives

Reusing local control state lets a peer resume clear local restrictions.
Changing only mixer output still sends forbidden media packets.
Replacing the transport for a direction update discards valid source and timing state.

## Consequences

Direction updates preserve sockets, identifiers, and media task ownership.
Reconsider accepted offer changes when interoperability requires codec or address updates.

## Confirmation

Test every permission combination, local controls across peer updates, stale generations, retransmission, rejection rollback, and received audio.
Keep the record proposed until reference and transport checks pass.
