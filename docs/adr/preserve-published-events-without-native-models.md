---
date: 2026-11-21
status: proposed
---

# Preserve published events without native models

## Context

Asterisk 22.9.0 publishes 43 event types in its [event definitions](https://github.com/asterisk/asterisk/blob/22.9.0/rest-api/api-docs/events.json).
A dialplan change during a call closed the Rust client's event session and prevented it from observing later events.

## Decision

Decode each modeled event into its existing Rust type.
Preserve the published name and payload fields of each remaining event in a separate variant.
Keep the session error for malformed modeled events and unrecognized names.

## Alternatives

Discarding unfamiliar events would hide information from subscribers.
Modeling every payload now would add resource and transfer types that the server doesn't produce.

## Consequences

Call tracking uses modeled events to update call state.
Subscribers can inspect the fields of other published events as JSON.
Promote an event to a typed variant when a supported operation consumes its fields.

## Confirmation

Compare the published names with the reference image and exercise every preserved name in the fast gate.
Verify that a preserved event can precede a modeled event on the same WebSocket connection.
