---
date: 2026-11-16
status: proposed
---

# Discard prefetched audio after playback seeks

## Context

The storage worker prefetches three playback frames.
[Asterisk playback controls](https://docs.asterisk.org/Asterisk_22_Documentation/API_Documentation/Asterisk_REST_Interface/Playbacks_REST_API/) include restart, forward, and reverse.
Seeking the file alone leaves old audio in the queue.

## Decision

Keep file I/O on the storage thread under the existing playback lease.
Send at most eight pending seek commands without waiting in the media driver.
Give each accepted seek a checked, increasing epoch.
Tag prefetched frames and end markers with their epoch.
Discard older epochs before returning audio.
Keep the reader until its lease ends, including while the queue holds an end marker.
Clamp sample positions to validated file bounds.
Preserve cancellation, terminal read failures, and bounded queue admission.

## Alternatives

Replacing the source reuses existing opening logic but requires coordinating late attachment and completion under another reservation.
Draining the receiver before seeking leaves a race with the worker.

## Consequences

The adapter carries one epoch and a bounded command queue per playback.
Reconsider source replacement if playback formats require reopening to seek.

## Confirmation

Test queued old audio, queued end markers, repeated seeks, saturation, boundaries, cancellation, and read errors.
Verify received audio through native and Asterisk reference controls before declaring compatibility.
