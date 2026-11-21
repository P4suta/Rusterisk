---
date: 2026-11-21
status: proposed
---

# Diagnose current dialplan routes and channel lifetimes

## Context

An extension, its endpoint, and a channel identifier can differ.
[Asterisk selects a matching extension at each priority](https://docs.asterisk.org/Configuration/Dialplan/Pattern-Matching/).

## Decision

Resolve the configured route with the server's dialplan matcher.
Expose its steps, routing contacts, app handoff, and matching live channels through authenticated reads.
Report a dynamic destination when its resolution needs call variables.
Sample server packet counters twice for a single channel generation.
Report the hold source and the traffic in each direction.

## Alternatives

Guessing the endpoint from an extension misses routes through an app.
A single packet total gives no time interval for the observation.

## Consequences

A diagnostic can identify the next routing or media check.
Verifying end-to-end audio still requires a receiver that checks the signal.
Revisit route resolution if the server exposes per-call variable values to a diagnostic reader.

## Confirmation

Exercise exact and pattern routes, call handoff, expired contacts, channel reuse, and unavailable media data.
