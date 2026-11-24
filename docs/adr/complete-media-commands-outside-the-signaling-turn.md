---
date: 2026-11-24
status: accepted
---

# Complete media commands outside the signaling turn

## Context

Eleven media operations can each delay unrelated signaling responses for five seconds.

## Decision

Queue media commands in arrival order without waiting.
Omit replies used only for logging.
The responsible job receives the reservation receipt.
The call record holds the deadline for each pending attachment or direction change.
A numbered media notice completes its matching wait.
Ordinary notices and command completions each have 128 slots in one delivery queue.
A notice holds its slot until signaling reads it.
Expiry releases the attachment.
Eight slots hold WebRTC sessions while the command queue orders their slot numbers with releases.

## Alternatives

Reply futures polled by the turn hide the pending state from the wake report.
Commands sent by the control task give one call two unordered routes.

## Consequences

Each pending state orders events for its call.
Reconsider when one task owns signaling and control decisions.

## Confirmation

An unrelated `REGISTER` must receive a response during a silent attachment.
Expiry must end the waiting call with cause 21.
A replacement WebRTC link must survive the expired worker.
