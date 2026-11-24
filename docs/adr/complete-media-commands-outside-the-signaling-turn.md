---
date: 2026-11-24
status: proposed
---

# Complete media commands outside the signaling turn

## Context

The signaling task awaits media replies inside its turn at eleven places, each for up to five seconds.
One silent media task delays every unrelated SIP response.

## Decision

Queue each media command in the turn without waiting, which keeps command order.
A reply that is only logged goes away.
A reservation hands its receipt to the job that owns the operation.
An attachment or a direction change stays in the call record as a pending completion with a deadline.
The media task reports that completion in a notice naming the call.
A numbered answer completes only its own wait, and an expired wait releases its media.

## Alternatives

Reply futures polled by the turn hide the pending state from the wake report.
Commands sent by the control task give one call two unordered routes.

## Consequences

Events for one call can arrive during a pending attachment, so each pending state defines their order.
Reconsider when one task owns signaling and control decisions.

## Confirmation

Tests must show a `REGISTER` response while a media task never answers.
Tests must show that an unanswered attachment ends its call with cause 21.
