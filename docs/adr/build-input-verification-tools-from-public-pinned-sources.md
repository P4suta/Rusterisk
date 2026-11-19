---
date: 2026-11-16
status: proposed
---

# Build input verification tools from public pinned sources

## Context

The cached proof image lacks native media build tools.
Input fuzzing needs CMake, a C++ compiler, and the pinned nightly toolchain.
The [fuzz workflow](https://rust-fuzz.github.io/book/cargo-fuzz.html) also needs Rust source for sanitizer builds.

## Decision

Build an independent image from the pinned public Rust image in the [Dockerfile](../../containers/input-verification/Dockerfile).
Install the dated nightly with Miri and Rust source.
Pin cargo-fuzz 0.13.2 and cargo-mutants 27.1.0.
Verify [CMake 4.5.0](https://github.com/Kitware/CMake/releases/tag/v4.5.0) archives with the published architecture hashes.
Use Debian's [Clang package](https://packages.debian.org/bookworm/clang) for native compilation.
Pin its [sanitizer runtime](https://packages.debian.org/bookworm/libclang-rt-14-dev) and [symbolizer](https://packages.debian.org/bookworm/llvm-14) for native defect diagnostics.
Reuse the owned builder with explicit processor, memory, process, and time limits.
Keep build operations separate from campaigns and publication.

## Alternatives

Extending a private cached image would hide its installation recipe.
Installing tools on the shared host would couple unrelated work.
Building tools inside each campaign would spend its execution budget on setup.

## Consequences

Image creation requires network access.
Campaigns consume a locally inspected immutable image identity.
Rust fuzz instrumentation alone doesn't establish native C sanitizer coverage.

## Confirmation

Check builder limits and cleanup with rejecting fixtures.
Build and inspect the image before running native campaigns.
