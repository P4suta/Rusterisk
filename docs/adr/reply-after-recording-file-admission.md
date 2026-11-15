---
date: 2026-11-15
status: proposed
---

# Reply after recording file admission

## Context

[Asterisk 22.9 rejects an existing recording before returning success](https://github.com/asterisk/asterisk/blob/22.9.0/res/ari/resource_channels.c).
Rusterisk performs file operations on a dedicated thread.
Waiting inside the control actor would prevent cancellation and event processing.

## Decision

Reserve the recording's name and media task in the control actor.
Keep its HTTP reply pending while an independent task requests file creation and connects the recording sink.
Return success only after admission succeeds.
Return a typed conflict for an existing file.
Bound pending replies and expire admission after five seconds.
Cancel owned work when the requester disconnects or admission expires.
Continue processing other requests and media events throughout admission.

## Alternatives

Returning success before file creation would replace a required HTTP conflict with a later failure event.
Running file operations in the actor would block unrelated calls.
Independent HTTP orchestration would duplicate media ownership and cancellation.

## Consequences

Recording admission needs a bounded reply registry keyed by media task token.
Completion and cancellation must resolve each reply once.
Reconsider the deadline if supported storage requires longer admission.

## Confirmation

Test existing files, slow storage, requester disconnection, late admission, and concurrent call events.
Keep the record proposed until reference comparisons pass.
