---
date: 2026-11-16
status: proposed
---

# Verify client requests using a local server

## Context

Independent package consumers don't establish HTTP interoperability with an extracted server candidate.
The [Asterisk 22.9.0 contract](https://github.com/asterisk/asterisk/blob/22.9.0/rest-api/api-docs/bridges.json) provides bridge creation, inspection, and deletion.

## Decision

Keep server lifetime in the distribution runner.
Borrow its loopback address, fixture credentials, expected version, and reserved bridge identifier.
Pass credentials through child environment variables.
Generate an explicit HTTP consumer with the candidate lockfile's exact Tokio registry identity and a [current-thread runtime](https://docs.rs/tokio/1.54.1/tokio/runtime/struct.Builder.html#method.new_current_thread).
Keep the core MSRV consumer unchanged.
Check typed ping, information, and bridge responses.
After confirming absence, attempt deletion even when creation returns no usable response.
Preserve operation and cleanup failures within separate deadlines.

## Alternatives

A second server process duplicates lifetime management.
HTTP constructor checks leave requests unverified.

## Consequences

The HTTP consumer requires an isolated server and runtime validation.
Reconsider the reserved identifier contract before allowing shared servers.

## Confirmation

Reject nonloopback targets, changed runtime identities, existing bridges, and secret-bearing diagnostics.
Require actual candidate HTTP execution before declaring interoperability.
