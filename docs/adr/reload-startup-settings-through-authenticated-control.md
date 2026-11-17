---
date: 2026-11-18
status: proposed
---

# Reload startup settings through authenticated control

## Context

Operators need settings reload.
[Asterisk REST Interface](https://github.com/asterisk/asterisk/blob/22.9.0/rest-api/api-docs/asterisk.json) reloads individual modules.

## Decision

CLI uses SDK for `POST /rusterisk/configuration/reload` ([POST](https://www.rfc-editor.org/rfc/rfc9110.html#section-9.3.3)).
Keep management operations outside the Asterisk REST Interface catalog.
Use live credentials through [password input](https://curl.se/docs/manpage.html#-u), independent of replacement settings.
Read regular files from fixed startup directories.
Reject bodies, query parameters, concurrent reloads, and `read_only` users.
Require current authentication and Origin policy at serialized admission.
Own one blocking loader outside audio storage.
Share one deadline for loading and preparation.
Cancel disconnected callers before the first swap.
Require completion or service shutdown afterward.
Preserve calls through [prepared publication](prepare-settings-before-serialized-publication.md).
Return [204](https://www.rfc-editor.org/rfc/rfc9110.html#section-15.3.5) after all acknowledgements.
Return file, line, and reason for configuration failures.
Keep operational errors distinct.
Management requests expire after 30 seconds and ordinary requests after 10.
Embedded servers may omit reload sources.

## Alternatives

[202](https://www.rfc-editor.org/rfc/rfc9110.html#section-15.3.3) supports long operations with job tracking.
Library-only reload excludes binary operators.

## Consequences

Reap started loaders after cancellation and before Control exits.
Shutdown may exceed 10 seconds for trusted local filesystems.
Stalled filesystems have no hard timeout.
Audio [disk ownership](own-disk-io-on-one-dedicated-thread.md) remains separate.

## Confirmation

Test authorization, busy rejection, positions, cancellation, reaping, calls, and cleanup.
Reconsider asynchronous jobs for insufficient request lifetimes.
