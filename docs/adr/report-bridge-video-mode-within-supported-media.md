---
date: 2026-11-15
status: proposed
---

# Report bridge video mode within supported media

## Context

The approved initial scope covers audio and defers video.
[Asterisk bridge snapshots](https://github.com/asterisk/asterisk/blob/22.9.0/main/stasis_bridges.c) expose a video policy even without a video source.
The reference reports `talker` for a default mixing bridge.
Rusterisk implements audio mixing.

## Decision

Report `video_mode=none` for Rusterisk audio bridges.
Keep `video_source_id` absent without a source.
Compare the video values with each server's declared media scope before comparing the remaining bridge fields.
Normalize only creation time and the explicitly checked video policy.
Keep errors, membership, names, identifiers, and other metadata in the comparison.

## Alternatives

Reporting `talker` would describe an unimplemented video policy.
Implementing video switching expands the approved initial scope.

## Consequences

Audio callers keep their existing operations.
Callers requiring video continue using the reference until Rusterisk supports video.
Revisit the metadata difference when video policy becomes executable.

## Confirmation

Check each server's video mode, missing source, other metadata, duplicate creation, queries, and deletion.
Keep the record proposed until reference calls and audio comparisons pass.
