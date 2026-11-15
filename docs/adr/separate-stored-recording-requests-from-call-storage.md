---
date: 2026-11-15
status: proposed
---

# Separate stored recording requests from call storage

## Context

Stored recording requests share the storage worker with call history and active recording completion.
A management burst must leave those lifecycle command slots available.

## Decision

Keep a separate bounded lane for 16 stored recording commands.
Preserve the 64-command lane for call history and active audio work.
Use the existing coalesced wake slot for both lanes.
Drain lifecycle commands and pump audio between management operations and sound entries.
Stop admission and drain both lanes before joining the thread.

## Alternatives

Reserving credits in one queue avoids another lane but couples cleanup capacity to every management producer.
Another storage thread isolates blocking operations but adds connection and stream coordination.

## Consequences

Management admission has its own limit.
One filesystem operation can still delay the shared thread, as described in [the disk ownership decision](own-disk-io-on-one-dedicated-thread.md).
Reconsider thread ownership if observed disk delays exceed the audio budget.

## Confirmation

Saturate management admission and verify lifecycle capacity, bounded rejection, work ordering, and shutdown.
Keep the record proposed until storage fault and platform checks pass.
