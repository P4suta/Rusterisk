---
date: 2026-11-16
status: proposed
---

# Bound client channel variable history

## Context

[Channel snapshots](https://github.com/asterisk/asterisk/blob/22.9.0/rest-api/api-docs/channels.json) can include channel variables.
Early events can omit transport variables returned by channel creation.
Merging partial observations indefinitely would grow each call's retained metadata.

## Decision

Preserve missing variables across observations and prefer values from later events.
Keep at most 128 variables and one mebibyte of variable names and values per channel.
Validate the complete merge before changing a snapshot.
Report capacity exhaustion and release owned session work through existing cleanup.
Preserve Stasis exit and destruction observations even when metadata exceeds capacity.
Apply the same bound to creation responses.
Keep the record proposed until failure and cleanup tests pass.

## Alternatives

Replacing metadata loses transport addresses.
Preserving only known transport keys loses app variables.
Silent eviction can remove a variable needed to connect media.

## Consequences

Servers returning larger metadata sets exceed the client's supported capacity.
Reconsider the bound if measured app requirements need more variables.

## Confirmation

Test both event orders, partial updates, overflow, unchanged snapshots after rejection, and terminal cleanup.
