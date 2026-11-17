---
date: 2026-11-17
status: proposed
---

# Isolate Windows console signals in owned helper processes

## Context

Windows verification needs graceful shutdown from a consoleless parent.
[Console changes reset control handlers](https://learn.microsoft.com/en-us/windows/console/setconsolectrlhandler).
The [approved exception](assign-windows-command-trees-to-jobs-during-creation.md) permits only Job accounting.

## Decision

Create each service with `CREATE_NEW_CONSOLE`.
An owned helper [attaches](https://docs.rs/win32console/0.1.5/win32console/console/struct.WinConsole.html#method.attach_console) to its console.
After attachment, [`set_ctrl_handler(None, true)`](https://docs.rs/crate/winconsole/0.11.1/source/src/console/console_main.rs) makes only the helper ignore Ctrl+C.
Keep the helper's process-local ignore setting until exit, without adding a callback.
Send [`generate_ctrl_event(false, None)`](https://docs.rs/crate/winconsole/0.11.1/source/src/console/console_main.rs) as `CTRL_C_EVENT`, group 0.
Preserve parent handlers and explicit standard streams.
Hold the service's `Child` handle until the helper exits.
Assign service and helper Jobs during creation.

## Alternatives

[Windows ignores `CREATE_NEW_PROCESS_GROUP` with `CREATE_NEW_CONSOLE`](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags), so PID-targeted group delivery is unsuitable.
`CTRL_BREAK_EVENT` would require a server `ctrl_break` receiver.
Forceful Job termination provides cleanup without establishing graceful shutdown.

## Consequences

The design adds two dependencies and a helper executable.
Reconsider when windows-spawn supplies owned console signaling.

## Confirmation

Keep proposed until native tests verify receiver readiness, consoleless delivery, parent handler preservation, unrelated-process isolation, helper lifetime, actual zero service exit, and empty Jobs after timeout or cancellation.
