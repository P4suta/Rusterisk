---
date: 2026-11-15
status: proposed
---

# Publish recording replacements after sync

## Context

The household phone app requests recording with `ifExists=overwrite`.
Truncating its existing audio at admission would lose that recording if the replacement fails.

## Decision

Write replacement audio to a separate `.replacement` file in the recording directory.
Keep the completed recording available throughout capture.
Sync the replacement's header and samples before publication.
Close its handle and use [`std::fs::rename`](https://doc.rust-lang.org/std/fs/fn.rename.html) to replace the destination in the same directory.
Cancellation deletes only the replacement.
Startup repairs unfinished replacement headers and publishes them under the requested name.
The staging extension preserves the overwrite decision across restart.
Default recordings keep their separate `.partial` files and refuse replacement.

## Alternatives

Writing directly to the completed file would expose unfinished audio and destroy the previous recording on cancellation.
A separate policy sidecar would add another file to coordinate during recovery.

## Consequences

Replacement temporarily needs space for both recordings.
Readers can keep the previous file open where the filesystem permits replacement.
Reconsider publication if a supported filesystem can't replace open files.

## Confirmation

Test publication, cancellation, interruption, failed publication, and recovery with existing audio.
Keep the record proposed until platform checks pass.
