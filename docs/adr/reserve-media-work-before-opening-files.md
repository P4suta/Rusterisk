---
date: 2026-11-15
status: proposed
---

# Reserve media work before opening files

## Context

File operations can finish after a caller cancels playback or recording.
A late source must not restart cancelled audio or attach to a replacement call.

## Decision

Reserve a bounded playback or recording slot before requesting file I/O.
Bind the reservation to its call generation and media task token.
Attach a playback source or recording sink only while its reservation is waiting.
Cancellation and channel release remove the reservation.
Reject late attachments and drop their storage leases.
Expire a waiting reservation after five seconds.
Keep call decisions in the pure core and file operations on the dedicated storage thread.

## Alternatives

Task cancellation alone can't retract a command already accepted by another task.
Remembering cancelled tokens needs a separate retention policy.
Checking only channel identifiers can admit audio into replacement calls.

## Consequences

Pending work consumes one explicit slot.
Report capacity failure before file I/O begins.
Each adapter owns its sources and completion senders.

## Confirmation

Test cancellation before attachment, channel release, expiry, reused generations, and received audio.
Keep the record proposed until the integration checks pass.
