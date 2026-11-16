---
date: 2026-11-16
status: proposed
---

# Own Unix command groups through terminal cleanup

## Context

Killing Cargo alone leaves compiler descendants alive.

## Decision

Give each command a [process group](https://doc.rust-lang.org/std/os/unix/process/trait.CommandExt.html#tymethod.process_group).
Observe exits with [waitid NOWAIT](https://docs.rs/rustix/1.1.6/rustix/process/struct.WaitIdOptions.html#associatedconstant.NOWAIT).
Keep the leader unreaped until group termination prevents identifier reuse, including early leader exit.
The CLI owns a concrete CommandSupervisor and borrows it through command ports.
Use rustix 1.1.6 and [signal-hook 0.4.6](https://docs.rs/signal-hook/0.4.6/signal_hook/low_level/pipe/) without default features for process control and a nonblocking self-pipe covering SIGINT and SIGTERM.
Poll signals with the command deadline.
Create no helper thread.
Keep registrations until terminal CLI cleanup, then unregister and let process exit close descriptors.
The dependencies contain private unsafe and shared registry implementation.
Project code uses neither.

## Alternatives

Per-command handler removal [leaves termination signals ignored](https://docs.rs/signal-hook-registry/latest/signal_hook_registry/fn.unregister.html).
command-group's publisher ended maintenance.
process-wrap exposes dynamic child dispatch.
Individual PID termination misses descendants.

## Consequences

Assume children keep their inherited group.
Abrupt SIGKILL of the supervisor can leave children alive.
Windows requires a separate Job Object adapter.
The decision remains proposed pending native checks.

## Confirmation

Require timeout, interruption, early leader exit, repeated execution, cleanup failures, and existing caller regressions.
