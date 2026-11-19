---
date: 2026-11-16
status: proposed
---

# Use deflate archives for Windows server candidates

## Context

Windows server candidates need ZIP archives with bounded content verification.
[PKWARE](https://pkware.cachefly.net/webdocs/casestudies/APPNOTE.TXT) defines checksums and compression records.

## Decision

Pin [zip 9.7.3](https://github.com/zip-rs/zip2/tree/7d0d324886eb51cd0c22be21c13e63e77ed37fb4), licensed under `MIT` with Rust 1.88 support.
Select only `deflate-flate2` and reuse the pinned Rust compression implementation.
Keep concrete library types at the adapter boundary.
The dependency contains shared ownership, dynamic dispatch, and unsafe internals.
Project code uses neither erased interfaces nor library extraction helpers.
Write nine regular files with fixed timestamps and permissions.
Bound output and verify entry counts, paths, sizes, checksums, and candidate bytes before materialization.
Reject encryption, links, archive prefixes, and trailing records.

## Alternatives

Native ZIP tools reduce Rust dependencies but add platform-specific versions and invocation rules.
A project ZIP implementation would duplicate a maintained format parser.

## Consequences

The dependency extends the archive trust boundary.
Reconsider the library if its maintenance or bounded verification contract becomes unsuitable.

## Confirmation

Require deterministic roundtrips, malformed-input rejection, failure propagation, dependency checks, and native Windows startup.
Keep the decision proposed until candidate verification passes.
