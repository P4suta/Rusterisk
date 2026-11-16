---
date: 2026-11-16
status: proposed
---

# Read gzip archives through bounded regular entries

## Context

Vendor extraction trusts one fixed publisher archive hash.
Distribution verification accepts different candidate archives.

## Decision

Pin tar 0.4.46 and flate2 1.1.11 with default features off.
Select flate2's explicit miniz_oxide feature.
Both use `MIT OR Apache-2.0` and support the xtask toolchain.
[tar](https://github.com/composefs/tar-rs/blob/fc459c149f83bf4daceaa52e17d351989002e1a9/src/archive.rs) keeps interior mutability and dispatch internally.
[flate2](https://github.com/rust-lang/flate2-rs/blob/cd06529164c5447958f309e649ffe6c9d6329865/src/ffi/miniz_oxide.rs) keeps boxed decoder state and unsafe helpers inside the dependency.
Project code uses concrete streaming readers without lint exceptions.
Inspect raw tar entries before writing regular files into an owned directory.
Bound compressed bytes, expanded bytes, file size, entry count, and path depth.
Reject links, devices, extensions, traversal, duplicate destinations, concatenated gzip members, and trailing data.
Validate gzip integrity and tar termination without calling unpack.

## Alternatives

Reusing system tar avoids dependencies.
Candidate inspection adds a separate protocol and native version control.

## Consequences

Archive libraries extend the dependency trust boundary.
Reconsider system tar when pinned native implementations meet the same contract.

## Confirmation

Exercise malformed headers, paths, file types, duplicates, resource limits, gzip integrity, concatenation, and cleanup with rejecting fixtures.
Verify real Cargo packages and independent consumers.
