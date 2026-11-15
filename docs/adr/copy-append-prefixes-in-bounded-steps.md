---
date: 2026-11-15
status: proposed
---

# Copy append prefixes in bounded steps

## Context

[Asterisk recording](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_stasis_recording.c) accepts `ifExists=append`.
Copying a large recording in one storage command would delay other recordings.

## Decision

Prepare append recordings in separate `.append` files.
Copy at most 8192 bytes per storage turn before admitting incoming audio.
Keep the completed recording readable until publication.
The initial header declares the complete existing prefix length.
Recovery discards a shorter copy and preserves the completed recording.
Recovery publishes a complete prefix with any captured audio through the replacement procedure.
Cancellation deletes only the staged append.

## Alternatives

Appending directly avoids copying but makes cancellation destructive.
A separate copy thread avoids worker delays but requires another lifetime and shutdown protocol.

## Consequences

Admission latency depends on the existing recording's size.
The storage worker continues other commands between copy steps.
Reconsider copying if admission deadlines reject normal recording sizes.

## Confirmation

Test bounded steps, received sample order, cancellation, interrupted copying, complete-prefix recovery, and concurrent storage work.
Keep the record proposed until platform and interruption checks pass.
