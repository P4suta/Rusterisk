---
date: 2026-11-16
status: proposed
---

# Assign Windows command trees to jobs during creation

## Context

Windows supervision must own descendants after their leader exits.
[Assigning a suspended process later](https://devblogs.microsoft.com/oldnewthing/20230209-00/?p=107812/) leaves a creator failure window.

## Decision

Require creation through `PROC_THREAD_ATTRIBUTE_JOB_LIST` using a reviewed publisher's safe interface.
Prefer [windows-spawn's concrete handles](https://docs.rs/windows-spawn/0.1.0/windows_spawn/struct.Job.html), conditional on a safe `Job::active_processes() -> io::Result<u32>` query.
Keep deadlines and empty polling in the project guard.
After normal leader exit, timeout, or cancellation, call `Job::terminate` and poll within a deadline for zero active processes.
Keep and close process and job handles explicitly.
Replace opaque `std::process::Command` stream settings with a typed command and stream request before platform spawning.
Keep console listeners concrete and scoped to the command supervisor.

## Alternatives

process-wrap returns erased child ownership.
command-group assigns jobs after spawning.
[Firehazard provides accounting](https://docs.rs/firehazard/latest/firehazard/job/fn.query_information_job_object.html) through a broader interface with publisher soundness reservations.

## Consequences

Publisher changes and command migration require separate review.
Reconsider after compatible safe APIs become available.
The project retains its unsafe prohibition.

## Confirmation

Require native tests for early leader exit, immediate descendants, spawn failure, timeout, console cancellation, and handle closure.
Keep proposed until API approval and native verification.
