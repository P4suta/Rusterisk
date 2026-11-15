---
date: 2026-11-16
status: proposed
---

# Validate control packet layout before borrowed parsing

## Context

[RFC 3550](https://www.rfc-editor.org/rfc/rfc3550.html#section-6.5) defines the Real-time Transport Protocol (RTP) and its RTP Control Protocol (RTCP).
Descriptions use counted chunks with aligned terminators.
[rtcp-types 0.3.0](https://github.com/ystreet/rtcp-types/blob/77d04d22603013b69ebe593f14c622719776802d/src/sdes.rs) consumes leading zeros from later source identifiers and accepts mismatched counts.
Padding can supply bytes required by packet bodies.

## Decision

Validate the bounded original compound layout before borrowed parsing.
Check unpadded body lengths, chunk counts, terminators, and final packet padding.
Use fixed scratch buffers for unpadded packets and single-chunk descriptions.
Change only count, padding, and length fields in internal headers.
Keep each source identifier and payload byte unchanged.
Keep borrowed packet parsers for other types and the existing writers.

## Alternatives

Unchanged parsing rejects valid descriptions and accepts malformed layouts.
A dependency fork adds provenance maintenance.
A complete replacement duplicates unrelated packet semantics.

## Consequences

Maintain a small layout boundary alongside the pinned parser.
Reconsider after a published parser passes identical wire fixtures.

## Confirmation

Require zero and leading-zero sources, chunk counts, misplaced padding, truncated bodies, excess data, and scratch bounds.
Confirm received audio continues after rejected control packets.
