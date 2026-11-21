---
date: 2026-11-21
status: proposed
---

# Observe calls while the Stasis app retains control

## Context

Opening a second connection for an existing Stasis app replaces its event connection.
An operator needs to observe calls while the telephone app keeps control.

## Decision

Capture the initial snapshot and subscribe to a bounded event feed in the same control turn.
Bind a channel observation to its generation.
Project timestamps, identifiers, signaling state, and event names into [newline-delimited JSON](https://github.com/ndjson/ndjson-spec).
End an interrupted observation with an explicit gap.

## Alternatives

Polling misses transitions between samples.
Registering the observer as a Stasis app would grant it call control and tie calls to its connection.

## Consequences

Readers can fall behind under sustained load.
After a gap, the reader can take a fresh snapshot.
Reconsider the bounded feed if operators require replay across disconnects.

## Confirmation

Observe simultaneous calls, hangup, identifier reuse, and a slow reader.
Disconnect the observer and verify the original app still controls the call.
