---
date: 2026-11-26
status: proposed
---

# Reserve media notices at admission

## Context

All calls share one media task.
Reporting the first received frame through a full notification queue can end the task while both phones still have working audio sockets.

## Decision

Reserve notification capacity when admitting a media leg or a command that promises a completion.
Return each reservation when signaling reads the notice or the operation ends without using it.
Carry the call generation and the operation token through delivery.
Count capacity refusals at admission and report closure when the receiving service stops.

## Alternatives

Waiting for queue space in an audio turn delays every phone.
Discarding a completion leaves signaling waiting for an operation that has ended.

## Consequences

An unread completion can delay a later admission.
Reconsider the reservation limits if a new transport needs more simultaneous notices per call.

## Confirmation

Fill the notification queue and check that both phones still receive audio.
Check that completion, hangup, and reuse of the released resources succeed under load.
