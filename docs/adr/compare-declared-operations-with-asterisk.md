---
date: 2026-11-14
status: accepted
---

# Compare declared operations with Asterisk

## Context

Existing applications depend on Asterisk REST Interface (ARI) responses, events, and call behavior.
[Asterisk channel operations](https://docs.asterisk.org/Asterisk_22_Documentation/API_Documentation/Asterisk_REST_Interface/Channels_REST_API/) define the public compatibility target.

## Decision

Use Asterisk 22.9.0 as the reference and declare support per operation.
Generate API descriptions from the same operation contract used by the server.
Return an explicit unsupported response for known operations without adapters.
Declare call compatibility only after reference and audio tests pass.

## Alternatives

A smaller custom interface would reduce implementation work but require clients to migrate.
Claiming compatibility for successful control responses would leave audio and event ordering untested.

## Consequences

Compatibility tests compare responses, error conditions, events, and received audio.
Reconsider a contract only through a successor decision with migration and acceptance tests.

## Confirmation

The approved plan accepts the reference version and compatibility boundary.
Contract tests validate schemas and errors.
Integration tests compare the same scenarios on both servers.
