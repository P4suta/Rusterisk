---
date: 2026-11-17
status: proposed
---

# Gate generated channel audio independently of conversation

## Context

[Asterisk channel Playback](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_stasis_playback.c) streams audio without sending UNHOLD.
Its [file adapter](https://github.com/asterisk/asterisk/blob/22.9.0/main/file.c) stops the existing generator and writes file frames through `ast_write`.
The [channel writer](https://github.com/asterisk/asterisk/blob/22.9.0/main/channel.c) doesn't reject audio solely for `AST_CONTROL_HOLD`.
Rusterisk's conversation gate suppresses channel Playback during local hold.

## Decision

Use a separate generated audio gate for channel Playback and music.
Preserve local hold while admitting generated audio to its destination.
Apply `channels.mute` and negotiated direction to both source kinds.
Keep conversation and bridge Playback behind the conversation gate.
Keep Waiting closed.
An explicit early media phase requires the transport admission in [the early media decision](own-early-media-before-answer.md).

## Alternatives

A music-only gate leaves ordinary Playback silent during hold.
Resuming the call for Playback would also reopen conversation.
Independent gates for each source kind duplicate the same permission.

## Consequences

Rename the music-specific permission and update its consumers together.
Source priority and lifetime remain separate decisions.
Reconsider the shared permission when a supported source requires different direction semantics.

## Confirmation

First reproduce silent channel Playback during hold using consumed known audio.
Check hold preservation, `channels.mute`, peer direction, bridge isolation, completion, and cleanup.
Compare received audio with Asterisk before accepting the record.
