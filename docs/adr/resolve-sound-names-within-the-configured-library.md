---
date: 2026-11-16
status: proposed
---

# Resolve sound names within the configured library

## Context

Asterisk supports names such as `digits/1` and language directories in [its file resolver](https://github.com/asterisk/asterisk/blob/22.9.0/main/file.c#L53-L63).
Channel identifiers can't represent those names.
The [sound resource](https://github.com/asterisk/asterisk/blob/22.9.0/res/ari/resource_sounds.c) reports language and codec pairs for library files.

## Decision

Give sound names a separate validated type with at most 128 ASCII bytes and eight relative components.
Reject empty components, traversal, absolute paths, encoded separators, and platform-specific separators.
Resolve playback and catalogue entries through the same library boundary.
Keep recording names as channel identifiers.

## Alternatives

Flat names preserve the current implementation but exclude ordinary Asterisk libraries.
Arbitrary filesystem paths support more Asterisk deployments but expose files outside the configured library.

## Consequences

Nested sound names become representable without changing channel identifiers.
Absolute sound paths remain an explicit compatibility restriction.
Reconsider when a deployment requires more trusted libraries.

## Confirmation

Check round trips, traversal rejection, ancestor links, missing files, and agreement between advertised sounds and received playback audio.
Keep the record proposed until reference playback comparisons pass.
