---
date: 2026-11-16
status: proposed
---

# Own Unix command groups through terminal cleanup

## Context

Killing Cargo alone leaves compiler descendants alive.

## Decision

The CLI lends its CommandSupervisor through command ports.
Start a [cat anchor](https://raw.githubusercontent.com/apple-oss-distributions/text_cmds/main/cat/cat.c) with a parent-owned input pipe and null outputs.
Join actual commands to its process group.
Keep the anchor unreaped to prevent identifier reuse.
Borrow services during bounded client commands.
Wait five seconds after SIGTERM for graceful shutdown.
Stop the group with SIGKILL, then reap both children with deadlines.
The anchor avoids macOS [zombie-group permission errors](https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/bsd/kern/kern_sig.c).
Use safe rustix 1.1.6 process APIs and [signal-hook 0.4.6](https://docs.rs/signal-hook/0.4.6/signal_hook/low_level/pipe/) without default features.
Poll a nonblocking SIGINT/SIGTERM self-pipe without helper threads.
Unregister and close descriptors at terminal CLI cleanup.
Dependencies contain private unsafe and registry sharing.

## Alternatives

Per-command handler removal [leaves termination signals ignored](https://docs.rs/signal-hook-registry/latest/signal_hook_registry/fn.unregister.html).
A self-executable anchor adds entrypoint wiring.
System cat already waits for input on supported Unix hosts.
command-group lacks maintenance.
process-wrap exposes dynamic dispatch.

## Consequences

Assume children keep their inherited group.
Abrupt SIGKILL of the supervisor can leave children alive.
Windows requires a separate Job Object adapter.

## Confirmation

Require child reaping, graceful shutdown, expiry, drop, early anchor exit, spawn failure, interruption, descriptor closure, and caller regressions.
