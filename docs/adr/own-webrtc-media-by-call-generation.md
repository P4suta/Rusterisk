---
date: 2026-11-15
status: proposed
---

# Own WebRTC media by call generation

## Context

[Datagram Transport Layer Security (DTLS) with Secure Real-time Transport Protocol (SRTP)](https://www.rfc-editor.org/rfc/rfc5764) authenticates and encrypts audio.
[Interactive Connectivity Establishment (ICE)](https://www.rfc-editor.org/rfc/rfc8839) and [DTLS negotiation](https://www.rfc-editor.org/rfc/rfc8842) use Session Description Protocol (SDP) messages.

## Decision

Own one str0m 0.25 `Rtc`, User Datagram Protocol (UDP) socket, and bounded channels per call generation.
Enable only AWS-LC cryptography.
Separate plaintext Real-time Transport Protocol (RTP) from WebRTC with closed enum variants.
Keep bounded descriptions out of diagnostics.
Verify [fingerprints](https://www.rfc-editor.org/rfc/rfc8122) and negotiate RTP Control Protocol (RTCP) multiplexing before accepting audio.
Resolve media-level security attributes before invoking str0m.

## Alternatives

Plaintext passthrough omits authentication and encryption.
Shared state weakens cancellation.
A separate media service adds a deployment boundary.

## Consequences

Drain str0m output after each mutation.
Handshake readiness and received audio remain separate observations.
Reconsider shared transport when a call needs two media streams.

## Confirmation

Tests must reject malformed descriptions and fingerprint mismatches, exercise cancellation, and compare received audio in both directions.
`SIP.js` browser calls remain required.
Status stays proposed pending integration and review.
