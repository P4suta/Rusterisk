---
date: 2026-11-15
status: proposed
---

# Use bridge generations for media

## Context

Clients can reuse a public bridge identifier after deletion.
A bridge can also lose every channel without ending its playback or recording.
The signaling adapter forgets its private bridge number when membership becomes empty.

## Decision

Use the pure control core's unique bridge generation as the media identifier.
Carry the generation in every membership, playback, recording, and deletion effect.
Keep membership updates separate from deletion.
Delete owned media only for the matching generation.
Use the same generation source for app bridges and dialplan bridges.

## Alternatives

A permanent adapter map would need its own deletion protocol and duplicate the core's ownership decisions.
Public identifiers alone could attach late work to replacement bridges.

## Consequences

Adapters no longer create bridge identities.
Empty bridges preserve owned media until completion or explicit deletion.
Reconsider the generation space if independent controllers must share one media process.

## Confirmation

Test empty membership, explicit deletion, identifier reuse, late completions, and received bridge audio.
Keep the record proposed until integration checks confirm those boundaries.
