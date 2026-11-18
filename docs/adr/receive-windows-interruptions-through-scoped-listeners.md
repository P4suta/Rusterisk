---
date: 2026-11-18
status: proposed
---

# Receive Windows interruptions through scoped listeners

## Context

Windows verification needs interruption while external Jobs keep command trees.
Unix supervision keeps interruption local to each supervisor.

## Decision

Keep [creation-time Job assignment](assign-windows-command-trees-to-jobs-during-creation.md) and `DropPolicy::Detach`.
Create one [Tokio `CtrlC`](https://docs.rs/tokio/1.54.1/tokio/signal/windows/struct.CtrlC.html) per supervisor before spawning children.
Enable [Ctrl+C processing](https://learn.microsoft.com/en-us/windows/console/setconsolectrlhandler) after subscribing because a launcher can pass down an ignore setting.
Poll with `Context::from_waker(Waker::noop())` in bounded synchronous waits without constructing a runtime.
Latch interruption locally, reject further ordinary commands, and continue bounded cleanup.
Keep the receiver through final interruption checks and Job cleanup.
A later supervisor starts a fresh scope, matching Unix.
Dispatch the console helper before parent listener setup.
Change console attachment only inside the helper.

## Alternatives

A [`ctrlc` closure](https://docs.rs/ctrlc/3.5.2/ctrlc/fn.set_handler.html) needs one cached registration across supervisors.
A global interruption latch would change fresh-scope behavior.
[Handler ordering](https://learn.microsoft.com/en-us/windows/console/setconsolectrlhandler) can preempt either provider.
Don't install both providers.

## Consequences

The [Windows handler](https://github.com/tokio-rs/tokio/blob/b53b2001ed2ccfd9b87e32a0c6da1a1eebdc80b8/tokio/src/signal/windows/sys.rs#L133-L140) returns `FALSE` when broadcast reports no receivers.
Dependency-internal registry sharing doesn't add shared domain state.
Add no managed `Arc` or `unsafe`.
Reconsider if polling or handler composition changes.

## Confirmation

Keep proposed until native tests cover polling without a runtime, concurrent supervisors, interruption during cleanup, fresh scopes, receiver-drop races, helper isolation, and empty Jobs.
