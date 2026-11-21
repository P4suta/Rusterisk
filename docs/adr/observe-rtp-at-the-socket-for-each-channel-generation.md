---
date: 2026-11-21
status: proposed
---

# Observe RTP at the socket for each channel generation

## Context

A media connection records the first decoded frame.
A UDP send establishes local transmission.
A peer report establishes reported reception.
[Asterisk increments its counters at the RTP socket](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_rtp_asterisk.c).

## Decision

Keep one bounded snapshot per attached RTP link and look it up with the live channel generation.
Count valid inbound RTP datagrams from the negotiated peer and successful outbound RTP sends.
Preserve the source and sequence fields from a peer reception report for a transmitted synchronization source, as defined by [RFC 3550 section 6.4](https://www.rfc-editor.org/rfc/rfc3550#section-6.4).
Expose the snapshot through an authenticated management read.
Return unavailable observations explicitly.

## Alternatives

Reading the mixer would measure forwarded frames and omit losses before decoding.
Implementing the complete [ARI RTP statistics model](https://github.com/asterisk/asterisk/blob/22.9.0/rest-api/api-docs/channels.json) would also require jitter, round-trip time, and loss estimates for each source.

## Consequences

Each snapshot contains metadata.
Reconsider the snapshot boundary when several simultaneous RTP streams share one call leg.

## Confirmation

Compare the counters with datagrams at controlled peers and use a reception report that names the transmitted source.
Check an idle interval, a retired channel, and reuse of a public channel identifier.
