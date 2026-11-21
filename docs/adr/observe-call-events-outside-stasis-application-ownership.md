---
date: 2026-11-21
status: proposed
---

# Observe call events outside Stasis application ownership

## Context

Opening a second connection for an existing Stasis application replaces its event connection.
An operator needs to observe the active telephone application and leave its calls under application control.

## Decision

Capture the initial snapshot and subscribe to a bounded event feed in the same control turn.
Bind a channel observation to its generation.
Project timestamps, identifiers, signaling state, and event names into [newline-delimited JSON](https://github.com/ndjson/ndjson-spec).
End an interrupted observation with an explicit gap.

## Alternatives

Polling misses transitions between samples.
Registering an observer as an application would add application privileges and disconnect semantics to a read-only operation.

## Consequences

Readers can fall behind under sustained load.
A gap tells the reader when to take a fresh snapshot.
Reconsider the bounded feed if operators require replay across disconnects.

## Confirmation

Observe simultaneous calls, hangup, identifier reuse, and a slow reader.
Disconnect the observer and verify the original application still controls the call.
