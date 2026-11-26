---
date: 2026-11-26
status: proposed
---

# Journal recording publication in SQLite

## Context

The storage worker can stop between changing an audio file and updating its catalog row.
The worker uses the same staging path during capture and after it trims the final samples.

## Decision

Record each operation and its destination in SQLite before changing the recording directory.
Keep the source name for copies and the final byte length for a recording ready for publication.
Synchronize completed audio before publishing it and acknowledge completion after the catalog and the journal commit together.
On startup, replay pending operations under the existing storage lock before accepting work.
Recover each append from a stage containing the original prefix and the new audio.

## Alternatives

Cancellation and interruption can leave the same file extension.
Keeping the audio in SQLite would move streaming reads and writes into the catalog transaction.

## Consequences

Starting and finishing a recording each require a durable metadata write.
Reconsider the storage boundary if two writers must operate on the same recording directory.

## Confirmation

Stop the process during capture, publication, and deletion.
Reopen the store twice and compare the catalog and the received samples after each restart.
