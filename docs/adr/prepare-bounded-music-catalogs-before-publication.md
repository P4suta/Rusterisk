---
date: 2026-11-17
status: proposed
---

# Prepare bounded music catalogs before publication

## Context

[Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_musiconhold.c) inventories names during initialization and opens streams during playback.
Listed audio can fail when selected.

## Decision

Use the existing 16 `StoredCommand` slots while pumping control and media.
`Empty` needs neither a root nor I/O.
`Files` owns checked settings and bounded metadata inventories without audio vectors.
As project policy, validate all classes before startup or reload publication.
Project limits are 64 `.wav` files per class, 256 track references, 512 visited entries, 16 MiB per file, and 64 MiB in total, including headers.
Read through one owned `File` and recheck observable metadata before publication.

## Alternatives

Deferred opening eases startup but postpones failures.
Immutable archives stabilize bytes at copying and lifecycle costs.

## Consequences

Every configured class needs valid assets before activation.
Keep active calls' starting inventories and order.
Preparation adds startup and reload I/O.
Atomic protection from hostile renames and audio immutability remain outside scope.
Reconsider limits or archiving when deployments exceed the profile.

## Confirmation

Test bounds, positioned errors, observable changes, cancellation, retained snapshots, received audio, and cleanup.
Keep proposed pending runtime and reference checks.
