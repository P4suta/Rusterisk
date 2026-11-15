---
date: 2026-11-15
status: proposed
---

# Stream stored recordings through bounded owned readers

## Context

A file response can outlive its admission request.
Loading a complete recording would tie memory use to its size.

## Decision

Reserve at most 16 stored file readers on the storage thread.
Each reader has three owned chunks of at most 8192 bytes.
Keep file descriptors on that thread and pump readers between lifecycle commands.
The consumer owns a lease whose release wakes the worker and closes its reader.
Require explicit completion and report truncated input as failure.
Bound each consumer read wait to five seconds.

Use Axum's fixed `Body::from_stream` boundary for the response.
Keep its internal body erasure inside the dependency.
Owned adapters use concrete stream and error types.

## Alternatives

Buffering complete files increases memory use with recording size.
Moving descriptors to asynchronous file wrappers adds another blocking work lifetime.

## Consequences

Slow consumers occupy bounded reader slots.
Reconsider admission and buffering if measured download latency prevents normal requests.

## Confirmation

Test exact bytes, completion, truncation, capacity, dropped consumers, authentication, and shutdown.
Keep the record proposed until transfer and platform checks pass.
