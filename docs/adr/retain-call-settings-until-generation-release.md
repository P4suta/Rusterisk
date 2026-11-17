---
date: 2026-11-17
status: proposed
---

# Keep call settings until generation release

## Context

The approved reload contract preserves starting settings for active and queued calls.
[Prepared publication](prepare-settings-before-serialized-publication.md) must also preserve later music selection without cloning the complete catalog per call.

## Decision

Control owns a bounded settings-generation arena.
Keep at most sixty-five committed generations and one prepared candidate.
Channels reference their starting generation until terminal release.
Capture selected native profiles before emitting effects.
Keep requested listener declarations separate from immutable bound addresses and TLS contexts.
Move profiles through queued effects and pending resolution without rereading current settings.
Keep starting music definitions while selections own only their selected class.
Registration retirement owns correlation metadata independently.
Reject generation saturation before cancellation or publication.

## Alternatives

Per-call catalog copies simplify ownership but multiply metadata.
Shared domain ownership conflicts with the approved implementation rules.
Reading current settings changes queued calls.

## Consequences

Terminal release must balance generation references.
Music files keep their existing validation contract.
Reconsider the bound when ordinary reloads encounter retained-generation saturation.

## Confirmation

Test queued effects, address resolution waits, replaced TLS contexts, starting class audio, stale releases, and saturation.
Keep proposed pending native verification.
