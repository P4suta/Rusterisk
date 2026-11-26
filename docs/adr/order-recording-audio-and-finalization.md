---
date: 2026-11-26
status: proposed
---

# Order recording audio and finalization

## Context

The media worker hands audio to the storage thread without waiting for disk writes.
A recording may finish while disk writes stall and the control queue is full.

## Decision

Send audio, finish, and cancellation messages through one bounded queue in arrival order.
Reserve one terminal message when admitting a recording and limit each recording to three outstanding audio frames.
Move each frame inline and notify storage once for a batch of media writes.
Use a recording generation to reject messages left over from a previous use of a name.

## Alternatives

Separate audio and control queues let a busy control queue reject a finish request.
Draining the audio queue before each control request preserves order only after the finish request has entered the control queue.

## Consequences

Each of the 64 admitted recordings can occupy four queue slots.
Reconsider the frame allowance if disk measurements show that a supported recording workload exceeds three pending frames.

## Confirmation

Stall storage, fill the control queue, send three distinct frames, and request normal completion.
Read all samples from the resulting audio file and check that a stale cancellation preserves the next recording.
