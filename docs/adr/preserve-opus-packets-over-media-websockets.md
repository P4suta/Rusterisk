---
date: 2026-11-16
status: proposed
---

# Preserve encoded audio packet boundaries

## Context

[Asterisk's channel driver](https://github.com/asterisk/asterisk/blob/22.9.0/channels/chan_websocket.c) treats Opus as a packet codec and rejects buffering controls in passthrough mode.
The mixer consumes bounded decoded audio frames.

## Decision

Treat each binary WebSocket message as one complete Opus packet.
Use an owned bounded decoder to assemble short packets and split longer packets into mixer frames.
Keep transport message boundaries independent of WebSocket fragmentation.
Encode each outgoing mixer frame as one complete packet.
Report an optimal frame size of zero.
Reject buffering, markers, pause, resume, flush, drain reporting, and direction commands in packet mode.
Preserve authentication, connection generations, direction gates, and owned cancellation.

## Alternatives

Concatenating packet bytes would lose codec boundaries.
Treating compressed bytes as fixed audio blocks would corrupt decoding.

## Consequences

The app supplies complete packets at the intended cadence.
Codec limits reject packets outside the supported decoded duration and payload bounds.
Reconsider transport limits when the shared decoder supports larger packets.

## Confirmation

Check packet duration, boundary rejection, command errors, bidirectional known audio, and cleanup.
Keep reference comparisons distinct from native audio checks.
