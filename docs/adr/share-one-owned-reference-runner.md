---
date: 2026-11-17
status: proposed
---

# Share one owned reference runner

## Context

The [candidate contract](require-runner-outcomes-for-candidate-verification.md) requires native reference comparisons from captured sources.
The CLI owns subprocesses, while [detached Docker containers](https://docs.docker.com/reference/cli/docker/container/run/#detached-mode--d) run independently of the initiating command.
An outer timeout can interrupt cleanup.

## Decision

Move reference orchestration into one library runner shared by the CLI and captured-source verification.
Pass the source root and one immutable image explicitly.
Set each command's working directory without changing the process directory.
Keep node checks, a fresh server build, both SIPp adapters, and every Asterisk REST Interface (ARI) and transfer case mandatory.
Validate container ID, execution label, name, and image before controlling Docker resources.
Attempt owned cleanup after command failures and interrupts.
Construct a private reference outcome only after comparisons, supervisor cleanup, source comparison, and workspace removal succeed.

## Alternatives

Wrapping the existing CLI preserves orchestration but requires a second recovery owner for daemon resources.
Separate runners would duplicate case admission and cleanup rules.

## Consequences

Share command and resource adapters without public test performers or manual receipts.
Reconsider for comparisons through an independent remote worker.

## Confirmation

Reject missing cases, foreign resources, changed sources, failed commands, and incomplete cleanup.
Keep proposed until shared execution and native comparisons pass.
