---
date: 2026-11-15
status: proposed
---

# Keep related channel snapshots in owned events

## Context

[Asterisk's `Dial` contract](https://github.com/asterisk/asterisk/blob/22.9.0/rest-api/api-docs/events.json) includes a required peer and optional caller and forwarded channels.
The reference sends external media notifications without a caller.
The approved plan requires concrete, owned event variants.

## Decision

Keep channel snapshots inline in `EventKind`.
Register one `large_enum_variant` expectation for the public event enum.

## Alternatives

Heap indirection would shrink the enum but conflict with the approved ownership policy.
Dropping related channels would lose public contract data.
Storing identifiers alone would lose the event's historical snapshots.

## Consequences

The largest event determines each queue slot's size.
Existing event queues keep fixed capacities.
Reconsider the representation if measured event memory exceeds the integration budget.

## Confirmation

Contract tests check omitted callers and forwarding snapshots.
Reference comparisons check external channel admission and event decoding.
The record stays proposed pending review and integration measurements.
