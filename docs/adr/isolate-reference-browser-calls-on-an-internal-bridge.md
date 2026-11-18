---
date: 2026-11-19
status: proposed
---

# Isolate reference browser calls on an internal bridge

## Context

The [browser runner](verify-browser-calls-in-an-owned-loopback-container.md) verifies received tones and recorded samples in Chromium.
Comparing Asterisk 22.9.0 requires the same browser actions and audio inputs.
Asterisk excludes loopback addresses when gathering [Interactive Connectivity Establishment](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_rtp_asterisk.c#L3469-L3578) host candidates.

## Decision

Start the reference on a dedicated [isolated internal bridge](https://docs.docker.com/engine/network/packet-filtering-firewalls/#gateway-modes) without published ports.
Join the browser runner to the [reference container's network namespace](https://docs.docker.com/engine/network/#container-networks).
Use loopback addresses for the page, signaling, and Asterisk REST Interface (ARI).
Use public ARI operations for event subscriptions and recording downloads on both servers.
Close browser processes before removing the reference container and its bridge.
Bind comparison results to both image identities and the captured source.

## Alternatives

A loopback-only namespace supplies addresses that Asterisk excludes from its candidate list.
A combined image duplicates the reference build in the browser toolchain.

## Consequences

The reference needs [WebRTC endpoint settings](https://docs.asterisk.org/Configuration/WebRTC/Configuring-Asterisk-for-WebRTC-Clients/) and module readiness checks.
The runner needs a Docker Engine that supports isolated bridge mode.
Reconsider the network boundary when testing a deployed secure WebSocket endpoint.

## Confirmation

Require the expected tones in both directions and in the downloaded recording on each server.
Reject missing readiness, unequal image identities, and incomplete cleanup.
