---
date: 2026-11-16
status: proposed
---

# Use authenticated packets at the WebRTC media boundary

## Context

str0m's [Real-time Transport Protocol (RTP) mode](https://github.com/algesten/str0m/blob/5f2183ce7c67117cd1960dfeb11bed11fc188b66/src/lib.rs#L443) exposes authenticated headers and sending streams.
Sample events omit source identity, making media lookup ambiguous.
[Different clock rates](https://www.rfc-editor.org/rfc/rfc7160#section-4.1) require distinct sources with control reports.

## Decision

Use authenticated `Event::RtpPacket` input and `StreamTx::write_rtp` output.
str0m owns connectivity checks, encryption, and RTP Control Protocol (RTCP) reports.
Reuse owned `RtpSender`, `RtpReceiver`, `TelephoneSender`, and `TelephoneReceiver`.
Bind generated packets to declared sources.
Use distinct synchronization sources for distinct clocks.
Own extended sequence counters across rollover.
Bound each call to two sending sources.
Clear pending media on cancellation and direction changes.

## Alternatives

Frame mode with separate scheduling still requires source lookup.
The complete-event writer rejects accepted short control timing.
Raw debug events copy every payload.

## Consequences

Own bounded playout and measure its latency.
Preserve description negotiation and fingerprint verification.

## Confirmation

Require secure duplex audio across supported codecs, legal packet durations, reordering, distinct clocks, repeated endings, event masks, cancellation, and direction changes.
Browser and reference checks remain required.
