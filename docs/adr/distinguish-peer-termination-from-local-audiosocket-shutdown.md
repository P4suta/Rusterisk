---
date: 2026-11-17
status: proposed
---

# Distinguish peer termination from local AudioSocket shutdown

## Context

The [AudioSocket definition](https://docs.asterisk.org/Configuration/Channel-Drivers/AudioSocket/#types) accepts socket closure as termination.
The [pinned receiver](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_audiosocket.c#L305-L310) recognizes socket closure as hangup.
An unconditional reply after peer closure can fail during shutdown.

## Decision

Return a closed terminal enum carrying statistics for local stop or observed peer termination.
After complete frames, close without another write when the peer closes or sends `AudioSocket::Terminate`.
Keep the existing `AudioSocket::Terminate` message for local stop.
Preserve incomplete frames, resets, and failed writes as errors.
Keep primary and cleanup failures together.

## Alternatives

Unconditional replies can fail after valid termination.
Ignoring `BrokenPipe` hides genuine write failures.
Closing local sockets directly also fits the reference but changes the existing `AudioSocket::Terminate` behavior.

## Consequences

Successful peer termination no longer requires a redundant write.
Local stop still reports failed termination writes during concurrent peer closure.
Reconsider when the reference termination contract changes.

## Confirmation

Native tests must reject replies after peer closure or `AudioSocket::Terminate`, preserve incomplete frames and reset errors, and verify local termination and failed writes with bounded cleanup.
