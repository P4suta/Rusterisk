---
date: 2026-11-26
status: proposed
---

# Select release optimizations by measured size

## Context

The server strips symbols and enables thin link-time optimization in its [Cargo release profile](https://doc.rust-lang.org/cargo/reference/profiles.html).

## Decision

Compare size-oriented optimization and the current profile on the same target with the same locked dependencies.
Measure the executable bytes and anonymous resident memory.

## Alternatives

The current profile prioritizes execution speed and already strips symbols.

## Consequences

A smaller executable justifies a change only if the first call receives both tones within the baseline time and 64 disconnects finish within 20 ms.

## Confirmation

Use development builds with the selected profiles for the comparison.
Check package contents and actual release executables during release verification.

On Linux ARM64, the speed profile produced 23,835,384 bytes and used 2,552 KiB of anonymous memory before the first call.
Size optimization produced 17,085,296 bytes and used 2,576 KiB, with both profiles recording 18 context switches per minute.
In two comparisons with reversed launch order, both tones arrived after 1,304 and 1,260 ms with the speed profile and after 1,313 and 1,210 ms with size optimization.
Keep the speed profile because the smaller binary missed the first-call criterion in one comparison.
