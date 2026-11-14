---
date: 2026-11-15
status: proposed
---

# Pin reviewed transport adapter sources

## Context

The published dependency graph includes unmaintained `pin-utils` through `nix`.
[RUSTSEC-2026-0441](https://rustsec.org/advisories/RUSTSEC-2026-0441) has no patched release.

## Decision

Pin the transport dependencies to commit `4405c7ec7c6cce5815ebde8ae20e7630bdaf4327` in the official repository.
The closed, unmerged [upstream proposal](https://github.com/webrtc-rs/webrtc/pull/953) disables unused Unix APIs in `webrtc-util`.
Use its matching `rtp`, `webrtc-srtp`, and `webrtc-util` packages together.

## Alternatives

The published graph includes the obsolete package.
Vendoring a manifest patch adds source maintenance.
Ignoring the advisory keeps the obsolete dependency.

## Consequences

Cargo pins the exact source revision.
Only the selected transport packages enter the build.
Reconsider the pin when a published release removes the obsolete dependency.

## Confirmation

Source review identifies the feature change.
Packet, authentication, replay, native build, and dependency checks must pass before acceptance.
