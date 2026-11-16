---
date: 2026-11-16
status: proposed
---

# Instrument native dependencies before boundary tests

## Context

Rust memory checks don't instrument bundled C dependencies.
Opus, SQLite, and AWS-LC execute across native boundaries.

## Decision

Rebuild the captured workspace with pinned Clang and combined address and undefined-behavior instrumentation.
Apply [AddressSanitizer](https://releases.llvm.org/14.0.0/tools/clang/docs/AddressSanitizer.html#usage) at compilation and final linking.
Apply [UndefinedBehaviorSanitizer](https://releases.llvm.org/14.0.0/tools/clang/docs/UndefinedBehaviorSanitizer.html#usage) with `-fno-sanitize-recover=all`.
Use a separate target directory and fail on the first diagnostic.
Require instrumented native archives, rejecting C probes, and nonempty production boundary tests.
Use one exit code per process because the [sanitizer runtimes share common flags](https://github.com/llvm/llvm-project/blob/llvmorg-14.0.6/compiler-rt/lib/asan/asan_flags.cpp#L64-L114).
Run bounded containers with an immutable image, isolated state, and network access only for dependency fetching.

## Alternatives

Rust-only instrumentation leaves native objects unchecked.
Uninstrumented system libraries don't establish coverage of bundled dependencies.

## Consequences

Sanitizer builds require separate compilation and runtime libraries.
Instrumented C tests supplement Rust checks and don't verify assembly or every possible execution.
Keep native checks at integration and release stages.

## Confirmation

Reject missing instrumentation, successful defect probes, incomplete inventories, skipped tests, diagnostics, and timeouts.
Exercise codec roundtrips, storage recovery, and verified TLS flows through production adapters.
