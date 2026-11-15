---
date: 2026-11-15
status: proposed
---

# Own disk I/O on one dedicated thread

## Context

Database and audio file operations can block independently of call control and audio delivery.
The store needs exclusive recovery ownership.

## Decision

Keep SQLite and file handles on one dedicated thread.
Follow [Tokio's guidance for persistent blocking work](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html).
Use bounded control messages and separate audio queues with three frames per stream.
Drain audio after every control message.
Coalesce wake messages when the control queue is full.
An audio queue overflow fails its operation explicitly.
Stop admission before draining accepted work and joining the thread.
Keep the store's file lock until its connection and streams close.

## Alternatives

Dedicated threads per stream isolate file delays but scale thread use with call count.
Direct writes in media tasks block audio delivery.

## Consequences

One thread serializes disk work.
Revisit the boundary if measured disk latency exceeds the audio queue budget.
SQLite's internal synchronization stays behind the owned store.

## Confirmation

Tests must cover queue overflow, failed writes, cancellation, shutdown, restart, and received recorded audio.
Keep the record proposed until those comparisons pass.
