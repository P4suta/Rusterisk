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
Expose its steps, routing contacts, application handoff, and matching live channels through authenticated reads.
Report a dynamic destination when its resolution needs call variables.
Sample server packet counters twice for a single channel generation.
Report observed hold separately from packet direction.

## Alternatives

Guessing the endpoint from an extension misses application routing.
A single packet total cannot show whether traffic continued during an interval.

## Consequences

A diagnostic can identify the next routing or media check.
Verifying end-to-end audio still requires a receiver that checks the signal.

## Confirmation

Exercise exact and pattern routes, call handoff, contact expiration, channel reuse, and unavailable media data.
