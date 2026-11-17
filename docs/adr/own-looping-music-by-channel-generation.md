---
date: 2026-11-17
status: proposed
---

# Own looping music by channel generation

## Context

[Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_musiconhold.c) loops class audio and remembers its consumed position between starts.
Music preserves hold status.
The [channel contract](https://github.com/asterisk/asterisk/blob/22.9.0/rest-api/api-docs/channels.json) requires applications to restart music after `/play` interrupts it.

## Decision

Bind music sources and saved cursors to channel generations and starting class snapshots.
Share bounded media admission and terminal release.
Select music as the channel's output source while preserving `channels.mute` and negotiated media direction.
Loop validated, nonempty class audio before publishing end-of-file.
Save the position actually consumed, excluding prefetched frames.
Admit `/play` before stopping music and require an explicit restart.
Report unavailable adapters or sources as identifiable failures.

## Alternatives

Repeated ordinary playbacks can introduce gaps and lose cursor ownership.
Mixing music with bridge audio would expose another participant's speech.
Mapping music to hold would close the audio gate.
A separate registry would duplicate lifetime decisions.

## Consequences

Preserve generation, capacity, and cancellation invariants.
Reconsider shared admission if music needs independent ownership.

## Confirmation

First reproduce the public API's current failure after known received audio.
Further tests must verify looping, output priority, cursor recovery, playback interruption, stale generations, saturation, and terminal cleanup.
Keep the record proposed pending implementation and reference comparison.
