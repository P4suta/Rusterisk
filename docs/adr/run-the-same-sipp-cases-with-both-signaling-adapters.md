---
date: 2026-11-17
status: proposed
---

# Run the same SIPp cases with both signaling adapters

## Context

The approved plan requires SIPp comparisons alongside the Rust client scenarios.
[SIPp 3.8.0](https://github.com/SIPp/sipp/releases/tag/v3.8.0) supports challenge-driven registration scenarios.
The existing reference container supplies an owned network and configuration lifetime.

## Decision

Build the pinned SIPp source archive within the reference image and verify its version.
Execute identical checked scenarios with Asterisk and an owned Rusterisk listener.
Start with authenticated registration and removal.
Use one call per scenario, finite message deadlines, and complete native call statistics.
Keep signaling completion distinct from received-audio comparisons.
Preserve bounded failure diagnostics and clean up processes, configuration, and listeners before accepting completion.
Keep image construction and actual comparison at the explicit integration stage.

## Alternatives

Host installation would introduce another shared executable and platform setup.
A separate image would add another container and network lifetime.
Rust fixtures alone don't meet the required independent client comparison.

## Consequences

The reference image includes another pinned build input.
Reconsider separate images when clients need independent lifetimes.

## Confirmation

Reject skipped calls, incomplete statistics, changed scenarios, failed versions, and cleanup failures.
Keep proposed until both real adapters pass the same scenario.
