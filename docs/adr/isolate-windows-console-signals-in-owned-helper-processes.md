---
date: 2026-11-17
status: proposed
---

# Isolate Windows console signals in owned helper processes

## Context

Windows verification must request graceful shutdown from a consoleless parent.
[Attaching or detaching a console resets control handlers](https://learn.microsoft.com/en-us/windows/console/setconsolectrlhandler), including handlers installed by Tokio.
The [approved exception](assign-windows-command-trees-to-jobs-during-creation.md) permits only the accounting query.

## Decision

Create supervised services with their own [console](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags).
Send control signals from a separate owned helper using published safe [attachment](https://docs.rs/win32console/0.1.5/win32console/console/struct.WinConsole.html#method.attach_console) and [signal](https://omarkmu.github.io/docs/winconsole/console/fn.generate_ctrl_event.html) APIs.
Keep the service handle alive until the helper exits.
Register the helper's stateless control handler after attachment and bound its lifetime to the helper process.
Preserve parent console state and explicit standard streams.
Assign service and helper jobs during creation.

## Alternatives

Changing the parent console risks invalidating existing handlers.
Standard input closure requires another server control contract.
Forceful Job termination provides cleanup without establishing graceful shutdown.

## Consequences

The design adds two Windows dependencies and a helper executable.
Reconsider when windows-spawn supplies an owned console signaling capability.

## Confirmation

Keep proposed until native tests verify consoleless delivery, unrelated-process isolation, handler lifetime, zero service exit, and empty Jobs after timeout or cancellation.
