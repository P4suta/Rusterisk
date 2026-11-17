---
date: 2026-11-17
status: proposed
---

# Keep call settings until generation release

## Context

The approved reload contract preserves starting settings for active and queued calls.
[Prepared publication](prepare-settings-before-serialized-publication.md) also preserves later music selection.

## Decision

Control owns sixty-five committed settings slots and one candidate.
The pure core decides reference lifetimes through terminal release.
Capture native profiles before emitting effects.
Control keeps queued profiles until signaling takes each profile once.
Effects carry the call key and settings generation.
Keep requested listeners separate from immutable bound addresses and TLS contexts.
Pending resolution uses captured profiles.
Channels keep starting music definitions without copying catalogs.
Registration retirement owns correlation metadata independently.
Reject saturation before cancellation or publication.

## Alternatives

Per-call catalog copies multiply metadata.
Shared ownership conflicts with project rules.
Inline profiles fail strict Clippy and enlarge every effect.
Current settings change queued calls.

## Consequences

Terminal release balances references and discards untaken profiles.
Reconsider the bound when ordinary reloads encounter saturation.

## Confirmation

Test queued effects, address resolution waits, replaced TLS contexts, starting class audio, stale releases, and saturation.
Keep proposed pending native verification.
