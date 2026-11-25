---
date: 2026-11-26
status: proposed
---

# Size worker pools from idle and disconnect measurements

## Context

The server configures two runtime workers and allows four blocking threads.
Storage has a dedicated thread.

## Decision

Compare worker counts under the same two-CPU limit.
Count threads and context switches for each thread during the same idle interval with [Linux process statistics](https://docs.kernel.org/filesystems/proc.html).

## Alternatives

The current pool permits blocking work and signaling to progress independently.

## Consequences

Reduce a pool only if thread count or idle switches fall, first-call bidirectional audio meets the baseline, and all 64 disconnects finish within 20 ms.

## Confirmation

Measure the last disconnect and the later received tone.
Repeat when adapters add blocking work.
