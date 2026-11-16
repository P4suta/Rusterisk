---
date: 2026-11-16
status: proposed
---

# Assign Windows command trees to jobs during creation

## Context

Windows supervision must own descendants after their leader exits.
[Assigning a suspended process later](https://devblogs.microsoft.com/oldnewthing/20230209-00/?p=107812/) leaves a creator failure window.

## Decision

Require `PROC_THREAD_ATTRIBUTE_JOB_LIST` creation through unchanged windows-spawn 0.1.0.
Use [published `Job::as_handle()`](https://docs.rs/windows-spawn/0.1.0/windows_spawn/struct.Job.html#impl-AsHandle-for-Job) and one approved [accounting query](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-queryinformationjobobject) with a `BorrowedHandle` and fixed stack buffer.
Before adoption, replace inherited `unsafe_code = "forbid"` with [`deny`](https://doc.rust-lang.org/rustc/lints/levels.html) only alongside a syntax gate and an exact registered local expectation.
Admit only the fixed query function.
Reject other unsafe syntax across cfg branches, macro tokens, and nested suppression attributes.
Keep deadlines, zero-process polling, typed streams, and explicit handle cleanup in the supervisor.

## Alternatives

Patching the publisher adds source provenance obligations for the same query.
process-wrap erases child ownership.
command-group assigns jobs after spawning.

## Consequences

All other project unsafe code remains prohibited.
Command migration and scoped console cancellation require separate review.
Reconsider when the publisher provides a safe accounting API.

## Confirmation

Require rejecting syntax fixtures and native lifecycle tests, including early leader exit, descendants, timeout, cancellation, and handle closure.
Keep proposed while command migration and graceful shutdown remain unverified.
