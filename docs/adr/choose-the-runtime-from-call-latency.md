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

The current-thread candidate used 3,464 KiB of anonymous memory, two threads, and 14 context switches per minute with a registered handset and one subscribed application.
The two-worker baseline used 4,228 KiB, four threads, and 13 switches per minute under the same workload.
Across two fresh starts in reverse order, both peers received their first tones after 1,283 and 1,386 ms on the current-thread runtime and after 1,215 ms on each baseline run.
Keep the multi-thread runtime.
