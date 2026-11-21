---
date: 2026-11-21
status: proposed
---

# Count media packets at the socket for each channel generation

## Context

A media connection records the first decoded frame.
A successful datagram send establishes local transmission.
A peer report establishes reported reception.
[Asterisk counts Real-time Transport Protocol (RTP) packets at the socket](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_rtp_asterisk.c).

## Decision

Keep one bounded snapshot per attached media link and look it up with the live channel generation.
Count valid inbound packets from the negotiated peer and successful outbound sends.
Keep the source and sequence fields from a peer reception report for a transmitted synchronization source, as defined in [the reception report fields](https://www.rfc-editor.org/rfc/rfc3550#section-6.4).
Expose the snapshot through an authenticated management read.
Use a null field for an unavailable observation.

## Alternatives

Reading the mixer would measure forwarded frames and omit losses before decoding.
The complete [Asterisk channel statistics model](https://github.com/asterisk/asterisk/blob/22.9.0/rest-api/api-docs/channels.json) also requires jitter, round-trip time, and loss estimates for each source.

## Consequences

Each snapshot contains metadata.
Reconsider the snapshot boundary when two media streams share one call leg.

## Confirmation

Compare the counters with datagrams at controlled peers and use a reception report that names the transmitted source.
Check an idle interval, a retired channel, and reuse of a public channel identifier.
