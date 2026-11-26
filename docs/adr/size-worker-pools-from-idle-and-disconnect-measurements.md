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

Repeat measurements when adapters add blocking work.

One worker used three threads, 3,912 KiB of anonymous memory, and 15 context switches in a minute with one registered handset and one subscribed application.
The two-worker baseline used four threads, 4,228 KiB, and 13 switches under the same conditions, below half the historical 10,841 switches per minute.
The single worker took 1,458 and 1,209 ms to deliver both tones across two fresh starts, compared with 1,215 and 1,202 ms for two workers.
The media service acknowledged all 64 simultaneous socket releases in 2.979 ms under a two-CPU limit.
Keep two runtime workers and the existing blocking pool.
