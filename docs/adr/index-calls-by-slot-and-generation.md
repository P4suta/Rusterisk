---
date: 2026-11-26
status: proposed
---

# Index calls by slot and generation

## Context

The server admits up to 64 calls.
A delayed response can arrive after a channel name returns to service.
The signaling adapter already carries the call generation in asynchronous events.

## Decision

Use the low six bits of the generation minus one to select a call slot.
Assign increasing generations and skip occupied slots.
Check the full generation before changing a slot.
The control boundary resolves channel names and supplies names for events.

## Alternatives

Sending a separate slot number would require adding a field to every asynchronous call event.
Looking up the channel name and generation in each worker repeats the work of the admission index.

## Consequences

Generation numbers can skip values when a slot remains busy.
Revisit the index width before increasing the admission limit.

## Confirmation

Exercise overlapping calls across a complete rotation of the slot index.
Deliver delayed events after reuse and verify that the new call survives.
