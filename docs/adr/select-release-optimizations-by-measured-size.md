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
