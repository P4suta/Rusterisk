---
date: 2026-11-26
status: proposed
---

# Share dialplan routes by settings generation

## Context

An incoming call copies every dialplan step even when two phones use the same settings.
A blind transfer can give a surviving call a route from a different settings generation.

## Decision

Build the dialplan index when preparing a settings generation.
Group routes by context and priority, then order exact extensions and patterns according to the Asterisk matching rules.
Keep a reference to the route generation until the last call using those instructions ends.
Use the same index for call admission, execution, transfer, Stasis continuation, and extension diagnosis.

## Alternatives

Looking up each step in the current configuration can redirect calls after a reload.
With a separate copy, each call needs memory in proportion to the dialplan size.

## Consequences

Reloading settings pays the cost of indexing routes once.
Revisit the lookup structure if new pattern rules or larger dialplans change the cost of matching.

## Confirmation

Check exact extensions and overlapping patterns with the [Asterisk pattern matching rules](https://docs.asterisk.org/Configuration/Dialplan/Pattern-Matching/).
Check route selection after a reload and transfer, and measure admission with 4,096 instructions.
