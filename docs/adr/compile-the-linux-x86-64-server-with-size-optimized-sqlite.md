---
date: 2026-11-27
status: proposed
---

# Compile the Linux x86-64 server with size-optimized SQLite

## Context

Median idle memory on Linux x86-64 reached 17,364 KiB against a baseline maximum of 17,284 KiB.
After the idle trial, the median reached 17,580 KiB against a maximum of 17,536 KiB.

## Decision

Use the [Linux ARM64 compilation settings](optimize-the-linux-arm64-server-and-sqlite-for-size.md) on Linux x86-64.
Compile the server with speed optimization and one code generation unit.
Select size optimization for SQLite with a [Cargo package override](https://doc.rust-lang.org/cargo/reference/profiles.html#overrides).

## Alternatives

The default release profile exceeded the memory limit.
Size optimization for the whole server increased CPU time in the ARM64 trials.

## Consequences

One unit limits parallel compilation of the server crate.
Revisit the overrides if CPU time, memory, or first audio exceeds the baseline after a compiler or dependency change.

## Confirmation

Compare both packages in [five alternating trials per load](../reference-comparisons.md#compare-server-load) on the same two Linux processors.
