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
Separate plaintext Real-time Transport Protocol (RTP) and WebRTC with closed, inline variants.
Keep descriptions out of diagnostics.
Verify [fingerprints](https://www.rfc-editor.org/rfc/rfc8122) and require RTP Control Protocol (RTCP) multiplexing.
Resolve media-level security attributes before invoking str0m.
Direction answers preserve local ICE credentials, fingerprints, `tls-id`, and DTLS role.
Reject transport replacement during direction changes.

## Alternatives

Plaintext passthrough omits authentication and encryption.
Shared state weakens cancellation.
Separate services add deployment boundaries.

## Consequences

Drain str0m output after each mutation.
Distinguish handshakes from received audio.

## Confirmation

Test malformed descriptions, fingerprints, cancellation, direction security, and duplex audio resumption.
`SIP.js` calls and review remain required.
