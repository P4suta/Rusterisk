---
date: 2026-11-18
status: proposed
---

# Reload startup settings through authenticated control

## Context

Operators need complete settings reload.
[Asterisk](https://github.com/asterisk/asterisk/blob/22.9.0/rest-api/api-docs/asterisk.json) reloads individual modules.

## Decision

CLI delegates to SDK for `POST /rusterisk/configuration/reload` ([POST](https://www.rfc-editor.org/rfc/rfc9110.html#section-9.3.3)).
Define management operations outside the Asterisk REST Interface catalog.
Read regular local files from the fixed startup directory.
Reject request bodies and query parameters.
Require current authentication and Origin policy.
Reject `read_only` and busy requests.
Own one loading/applying caller.
Preserve calls through [prepared publication](prepare-settings-before-serialized-publication.md).
Cancel disconnected callers before the first swap.
Require completion or service shutdown afterward.
Return [204](https://www.rfc-editor.org/rfc/rfc9110.html#section-15.3.5) only after all acknowledgements.
Report file, line, and reason on failure.
Embedded servers may omit the required reload source.
Give reload its own timeout covering loading and publication.

## Alternatives

[202](https://www.rfc-editor.org/rfc/rfc9110.html#section-15.3.3) supports long operations but needs job tracking.
Library-only reload excludes binary operators.

## Consequences

CLI credential input remains unresolved for changed replacement passwords.
`Config::load` caps eight files at 1 MiB each without regular-file checks.
Started `spawn_blocking` work outlives cancellation.
Reconcile loading with [disk ownership](own-disk-io-on-one-dedicated-thread.md).

## Confirmation

Test platform parity, authorization, unavailable sources, busy rejection, disconnects, failed loads, continued audio, and cleanup.
Reconsider asynchronous jobs when request lifetimes prove insufficient.
