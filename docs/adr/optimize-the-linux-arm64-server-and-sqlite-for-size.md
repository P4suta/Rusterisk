---
date: 2026-11-26
status: proposed
---

# Optimize the Linux ARM64 server and SQLite for size

## Context

The recording journal and call handling changes increased resident memory in the Linux ARM64 package.
Optimizing SQLite alone still exceeded the idle memory limit.

## Decision

Use size optimization for the server and bundled SQLite in Linux ARM64 distributions through [Cargo package overrides](https://doc.rust-lang.org/cargo/reference/profiles.html#overrides).
Keep speed optimization for the media codec and mixer.

## Alternatives

An earlier [whole-program comparison](select-release-optimizations-by-measured-size.md) favored the speed profile for first-call audio.
Package overrides limit the change to two packages on one target.

## Consequences

Code size can change call setup time.
Revisit the overrides if a compiler or dependency update exceeds a CPU, memory, or audio latency limit.

## Confirmation

Five alternating trials per build used the same two Linux processors.
With 64 Real-time Transport Protocol legs, median resident memory fell from 18,948 to 17,040 KiB.
After hangup the median fell from 19,152 to 17,112 KiB.
At each load the candidate median for both audio directions stayed within the largest baseline time.
The [regression tests](../reference-comparisons.md#compare-server-load) also check CPU time and the 20 ms limit for 64 simultaneous disconnects.
