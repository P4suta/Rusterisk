---
date: 2026-11-16
status: proposed
---

# Assign Windows command trees to jobs during creation

## Context

Windows supervision must own descendants after leader exit.
[Assigning a suspended process later](https://devblogs.microsoft.com/oldnewthing/20230209-00/?p=107812/) leaves a creator failure window.

## Decision

Require `PROC_THREAD_ATTRIBUTE_JOB_LIST` creation through unchanged windows-spawn 0.1.0.
Keep workspace and xtask [`unsafe_code = "forbid"`](https://doc.rust-lang.org/rustc/lints/levels.html).
Confine the approved [accounting query](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-queryinformationjobobject) to private package `rusterisk-windows-job-accounting`.
Use [published `Job::as_handle()`](https://docs.rs/windows-spawn/0.1.0/windows_spawn/struct.Job.html#impl-AsHandle-for-Job), a `BorrowedHandle`, and a fixed stack buffer.
Accept only the query's complete root syntax tree and an exact [manifest](https://doc.rust-lang.org/cargo/reference/manifest.html).
Require `build = false`, no procedural macros, and only pinned windows-sys.
Turn off automatic target discovery.
Mirror [workspace lints](https://doc.rust-lang.org/cargo/reference/workspaces.html#the-lints-table) exactly, except the package's registered `unsafe_code = "deny"`.

## Alternatives

Lowering xtask to `deny` requires checking its generated inputs and macro expansions.
A published safe accounting API would remove the local exception but delay Windows supervision.

## Consequences

The boundary adds one private package and manifest validation.
Reconsider when a compatible published accounting API exists.

## Confirmation

Require rejecting syntax, manifest, and lint fixtures and native lifecycle tests, including early leader exit, descendants, timeout, cancellation, and handle closure.
Keep proposed while command migration and graceful shutdown remain unverified.
