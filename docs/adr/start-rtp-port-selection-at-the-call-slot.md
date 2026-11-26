---
date: 2026-11-27
status: proposed
---

# Start media port selection at the call slot

## Context

When 64 calls bind port pairs in an empty range, starting each search at the lowest port repeats 2,016 failed binds.
Signaling serves other calls between admissions.

## Decision

Use the [call slot](index-calls-by-slot-and-generation.md) to choose the first pair within the configured search window.
If another socket holds either port, try the remaining pairs in order and wrap at the end of the window.
Keep the existing limit of 256 pairs and let the operating system assign ephemeral ports.
Use an even port for Real-time Transport Protocol data and the following odd port for control reports, following [RFC 3550 section 11](https://www.rfc-editor.org/rfc/rfc3550#section-11).

## Alternatives

A cursor shared by listeners would also distribute binds but would couple independent call and listener lifetimes.
Searching from the lowest port visits the longest occupied prefix when the service is busy.

## Consequences

Concurrent calls can select the same first port when the range contains fewer pairs than call slots.
The socket bind resolves collisions with other processes and calls.

## Confirmation

Exercise all 64 slots, collisions at both ends of a range, and ephemeral ports.
Revisit the search window if external reservations routinely exhaust it.
