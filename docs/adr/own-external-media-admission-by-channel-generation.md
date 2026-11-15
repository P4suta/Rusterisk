---
date: 2026-11-15
status: proposed
---

# Own external media admission by channel generation

## Context

External media channels have no signaling dialog but need channel ownership and cleanup.
The [reference implementation](https://github.com/asterisk/asterisk/blob/22.9.0/res/ari/resource_channels.c) returns local transport variables after creating the media channel.
Rusterisk already separates control decisions from media effects.

## Decision

Keep external channels in the existing call core.
Give each admitted transport the channel's generation.
Keep transport state separate from signaling dialogs.
Reserve control capacity before binding or connecting.
Reply after transport admission supplies local variables.
Deliver Stasis, answer, and received-media progress through their existing independent paths.
Cancel the exact generation on admission failure, timeout, shutdown, or channel release.

## Alternatives

A synthetic signaling dialog would misrepresent protocol ownership.
Returning before transport admission would omit the local address clients need for audio.

## Consequences

Media-only channels share bridges, recording, playback, and control limits.
Admission receipts need bounded storage and a deadline.
Late completion must close its transport without reviving a channel.
Reconsider the boundary if a transport requires a different channel lifecycle.

## Confirmation

Check received audio, local variables, cancellation races, failures, and socket release with the reference.
Keep the record proposed until transport comparisons pass.
