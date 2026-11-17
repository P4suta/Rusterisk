---
date: 2026-11-17
status: proposed
---

# Prepare settings before serialized publication

## Context

The approved reload contract requires complete validation, starting settings for existing calls, and restart for listener changes.
Control, signaling, and transport adapters hold independent settings.
[Asterisk inbound event sessions](https://github.com/asterisk/asterisk/blob/22.9.0/res/ari/ari_websockets.c) remain outside outbound configuration observers.

## Decision

Runner coordinates one pending replacement.
Prepare owned settings, certificates, external clients, and music before publication.
Continue processing signaling, termination, and deadlines during preparation.
Compare requested startup and replacement listener settings.
Reserve generations and [registration retirement capacity](own-outbound-registrations-by-configuration-generation.md) before cancellation.
Pause new admissions only during serialized publication across adapters.
Keep fallible I/O outside publication.
Stop the service if a participant fails during publication.
Preserve starting profiles for active and pending calls, including later music selection.
Advance endpoint observation generations.
Apply authentication and Origin policy to new requests while preserving established inbound event sessions.
Update reload time only after publication.

## Alternatives

Independent replacements expose mixed generations.
Waiting inside an actor blocks required completion events.
Restarting terminates existing calls.

## Consequences

Preparation requires bounded owned snapshots.
Reconsider for listener migration.

## Confirmation

Test rejected preparation, saturation, cancellation, stale completions, participant failure, and calls spanning publication.
Keep proposed pending native checks.
