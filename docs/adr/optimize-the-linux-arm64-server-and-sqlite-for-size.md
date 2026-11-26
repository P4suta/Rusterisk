---
date: 2026-11-26
status: proposed
---

# Compile the Linux ARM64 server as one code generation unit

## Context

The recording journal and call handling changes increased resident memory in the Linux ARM64 package.
Optimizing SQLite alone still exceeded the idle memory limit.

## Decision

Build the Linux ARM64 server with speed optimization and one [code generation unit](https://doc.rust-lang.org/cargo/reference/profiles.html#codegen-units).
Optimize bundled SQLite for size through a [Cargo package override](https://doc.rust-lang.org/cargo/reference/profiles.html#overrides).

## Alternatives

An earlier [whole-program comparison](select-release-optimizations-by-measured-size.md) favored speed optimization for first-call audio.
Optimizing the server for size saved memory but exceeded the CPU limit in repeated comparisons at 64 connections.

## Consequences

Compiling one code generation unit limits parallel compilation of the server crate.
Revisit the overrides if a compiler or dependency update exceeds a CPU, memory, or audio latency limit.

## Confirmation

Five alternating trials per build used the same two Linux processors.
At 64 connections, median CPU time per second fell from 30.80 to 28.98 ms.
Median resident memory fell from 19,064 to 17,116 KiB during calls and from 19,268 to 17,316 KiB after hangup.
At each load, both audio arrival medians stayed within the largest baseline measurement for their direction.
The [regression tests](../reference-comparisons.md#compare-server-load) also enforce a 20 ms limit for 64 simultaneous disconnects.
