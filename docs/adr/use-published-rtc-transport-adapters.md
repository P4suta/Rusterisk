---
date: 2026-11-19
status: proposed
---

# Use published media transport adapters

## Context

The [reviewed Git pin](pin-reviewed-rtp-adapter-sources.md) removes the unused Unix dependency chain that reaches unmaintained `pin-utils`.
The published WebRTC.rs 0.17.3 graph still uses `nix` 0.26 through `webrtc-util`.
The [WebRTC.rs 0.21.1 release](https://github.com/webrtc-rs/webrtc/releases/tag/v0.21.1) publishes the corresponding transport crates with `nix` 0.31.

## Decision

Use `rtc-rtp`, `rtc-srtp`, `rtc-shared`, and `rtc-crypto` from the same published release.
Supply the concrete ring provider for the existing `AES_CM_128_HMAC_SHA1_80` profile.
Preserve packet payload bounds, per-stream context ownership, and replay windows.

## Alternatives

Keeping the reviewed Git revision preserves existing behavior and keeps the patch outside a published release.
Switching to the published 0.17.3 crates brings back the dependency chain.

## Consequences

Cargo can verify the transport crates with registry checksums.
Context creation takes the provider as an explicit input.

## Confirmation

Require the locked graph to exclude `pin-utils` and pass the dependency audit.
Compare protected packet bytes with the published [libSRTP test vector](https://github.com/cisco/libsrtp/blob/v2.7.0/test/srtp_driver.c#L1750).
Run packet and replay tests, reference call comparisons, and packaged server checks before accepting the migration.
