---
date: 2026-11-15
status: proposed
---

# Normalize packet audio before mixer admission

## Context

[Audio packet durations](https://datatracker.ietf.org/doc/html/rfc3551#section-4.2) can differ from the mixer's 20 ms period.
[Opus packets](https://datatracker.ietf.org/doc/html/rfc7587#section-4.2) can contain 120 ms of audio.
The mixer requires complete 20 ms frames.

## Decision

Decode bounded packets before assembling complete mixer frames at the negotiated sample rate.
Keep at most ten ready frames and one partial frame per stream.
Accept up to 200 ms of G.711 audio and 120 ms of Opus audio within a 2048-byte datagram bound.
Discard partial samples across timestamp discontinuities and negotiated direction changes.
Preserve packet ordering and source validation before decoding.

## Alternatives

Requiring 20 ms packets rejects valid peers.
Teaching the mixer each packet duration spreads transport timing into routing and recording.
An unlimited sample queue breaks the resource bound.

## Consequences

Long packets require more bounded buffering and increase sender latency.
Reconsider the bound when supporting another codec or packet transport.

## Confirmation

Verify short and long packets, known samples, wrapping timestamps, gaps, cancellation, queue bounds, and received bridge audio.
Keep the record proposed until reference checks pass.
