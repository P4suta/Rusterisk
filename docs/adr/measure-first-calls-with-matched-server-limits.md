---
date: 2026-11-19
status: proposed
---

# Measure first calls with matched server limits

## Context

The browser comparison embeds Rusterisk in the test container and runs Asterisk in a second container.
Performance measurements need the same process boundaries and limits.

## Decision

Start each packaged server in its own isolated container with equal processor and memory limits.
Reuse the same browser driver, client app, codec, and input recordings.
Measure request acceptance, answered channels, and the first observed tone in each direction.
Observe both directions concurrently with a monotonic clock.
Read processor usage from [the server control group](https://docs.kernel.org/admin-guide/cgroup-v2.html).
Sample resident memory for the owned server processes through [the Linux process files](https://docs.kernel.org/filesystems/proc.html).
Count setup and call actions alongside the source lines of the common client and each configuration.

## Alternatives

Timing the compatibility run measures the combined browser and server workloads under different resource allocations.

## Consequences

Prepare both images before measurement and restart with empty storage for each call.
Reconsider the collector if it reports an unexpected server process.

## Confirmation

Run one sequence per order with identical inputs and preserve each measurement.
Require matched audio and completed recordings for each trial.
