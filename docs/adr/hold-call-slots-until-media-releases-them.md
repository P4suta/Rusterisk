---
date: 2026-11-26
status: proposed
---

# Hold call slots until media releases them

## Context

A hangup can reach call control while a media command or stream task still owns the transport.
The channel name can return to service before the old transport closes.

## Decision

The core marks a call as ending and gives its caller one ordered teardown.
Call control cancels admissions, participants, recordings, dial branches, bridge membership, and media in order.
The media worker acknowledges a stream after its input and output task exits.
Only the matching generation can release the slot after teardown.

## Alternatives

Freeing the slot on hangup would let a new call claim transport capacity that the previous call still uses.
Waiting for the transport inside call control would delay work on unrelated calls.

## Consequences

At capacity, admission waits for the first media acknowledgement.
Revisit the order if another adapter takes ownership of a call resource.

## Confirmation

Check the teardown order, duplicate releases, and delayed acknowledgements with core tests and the model checker.
Fill all 64 slots and verify that the first acknowledgement permits exactly one replacement.
