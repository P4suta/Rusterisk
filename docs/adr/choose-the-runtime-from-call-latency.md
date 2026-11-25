---
date: 2026-11-26
status: proposed
---

# Choose the runtime from call latency

## Context

The server runs Tokio with two worker threads.
One thread could reduce idle scheduling at the cost of making a long media turn delay signaling.

## Decision

Compare the current-thread and multi-thread runtimes with identical binaries apart from the runtime selection.
Give each container two CPUs and the same calls.

## Alternatives

Keeping two workers permits signaling to progress while media runs under the [Tokio runtime builder](https://docs.rs/tokio/latest/tokio/runtime/struct.Builder.html).

## Consequences

Adopt a runtime change only when idle switches or resident memory fall and first-call audio arrives within the baseline time.

## Confirmation

Measure the later of the two received tones and require the last of 64 simultaneous disconnects to finish within 20 ms.
Repeat the comparison when the workload changes.
