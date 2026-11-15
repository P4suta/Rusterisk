---
date: 2026-11-16
status: proposed
---

# Fuzz input parsers with source-bound bounded targets

## Context

Compatibility parsers accept untrusted configuration, control messages, signaling, and media.
The [Rust Fuzz Book](https://rust-fuzz.github.io/book/cargo-fuzz.html) requires nightly Rust and a C++ compiler.
[libFuzzer](https://llvm.org/docs/LibFuzzer.html) provides input, execution, and memory bounds.

## Decision

Use cargo-fuzz 0.13.2 and libfuzzer-sys 0.4.13 through an independent manifest with production path dependencies.
Exercise valid and invalid seeds through settings, media control, Session Initiation Protocol (SIP), and media targets.
Check serialization round trips and equivalent fragmented streams.
Keep source read-only and copy harnesses into temporary writable state.
Run one target at a time with a fixed seed, finite runs, a time limit, and explicit processor and memory limits.
Reject empty inventories, missing native completion, crashes, and timeouts.
Preserve failing inputs for regression tests.

## Alternatives

Unit tests provide deterministic examples without discovering inputs.
Unbounded native fuzzing conflicts with shared resource limits.
Copied parser models could diverge from production.

## Consequences

Native media dependencies require CMake.
Instrumenting Rust alone doesn't establish native sanitizer coverage.
Reconsider the inventory when production gains another external parser.

## Confirmation

Run deterministic harness contracts and bounded campaigns on actual production dependencies.
Verify source identity and owned resource cleanup.
