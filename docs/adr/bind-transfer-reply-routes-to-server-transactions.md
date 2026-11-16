---
date: 2026-11-16
status: proposed
---

# Bind transfer reply routes to server transactions

## Context

A cached `REFER` reply can outlive its notification usage.
[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-17.2.2) retains non-INVITE server transactions for retransmissions.
Notification completion must preserve reply routing.

## Decision

Capture a payload-free Network route before Controller mutation.
Use the transaction identity and expiry retained by `Engine` after notification termination.
Keep listener, peer, and connection generation while either usage or cached reply remains.
Check peer and generation on every send.
Prune expired records before admission.
Bound each engine to its 64 `REFER` owner slots plus 256 server transaction slots.
Reject insertion before Controller mutation when capacity is unavailable.
Keep normal call, registration, and target lifetimes independent.

## Alternatives

An independent retired map with 64 slots uses less memory but duplicates expiry and needs separate admission checks.
Current-peer routing can send an old reply through a replacement connection.

## Consequences

Notification completion doesn't immediately release cached reply routing.
Keep metadata without copying packets.
Reconsider the decision after changes to the 64/256 limits or cache lifetime.

## Confirmation

Replay cached `202` after final `NOTIFY` settlement.
Deliver through the original flow and suppress replacement flows.
Exercise expiry, saturation, disconnect, and shutdown while preserving unrelated `INVITE`, `REGISTER`, and target audio.
