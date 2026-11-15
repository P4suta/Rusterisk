---
date: 2026-11-15
status: proposed
---

# Publish recording copies after complete bounded work

## Context

The [reference copy operation](https://github.com/asterisk/asterisk/blob/22.9.0/res/ari/resource_recordings.c) rejects existing destinations.
Copying a large file in one storage command can delay call cleanup and audio writes.

## Decision

Copy stored bytes through a private `.copying` file on the storage thread.
Reserve at most 16 copies and advance each by at most 8192 bytes between lifecycle commands.
Publish a synchronized, closed file through a hard link that refuses existing destinations.
Keep copies separate from live recording recovery.
Remove interrupted copy stages during startup.
Requester cancellation removes its stage and releases the source descriptor.

## Alternatives

A synchronous complete copy delays other storage work.
Publishing a recording stage during recovery would expose incomplete copied audio.
An asynchronous filesystem wrapper adds another blocking task lifetime.

## Consequences

Destination names stay reserved until completion or cancellation.
Copy requests share the management admission budget.
Reconsider the chunk budget if measured copy work delays audio.

## Confirmation

Test exact bytes, bounded steps, conflicts, cancellation, restart, and simultaneous recording writes.
Keep the record proposed until integration and platform checks pass.
