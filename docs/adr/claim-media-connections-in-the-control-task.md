---
date: 2026-11-15
status: proposed
---

# Claim media connections in the control task

## Context

[Asterisk 22.9](https://github.com/asterisk/asterisk/blob/22.9.0/channels/chan_websocket.c) publishes an ephemeral media connection identifier and rejects duplicate connections.
The approved plan requires authentication, Origin checks, bounded admission, and cleanup after cancellation.

## Decision

Generate random connection identifiers in an adapter and bind them to channel generations in the control task.
Treat the identifier as the bearer credential for `/media/{connectionId}`.
Validate transport security, Origin, and protocol before claiming the connection once.
Move an owned lease into the socket task.
Observe dropped leases through bounded lifetime channels so failed upgrades and queue pressure still release the generation.

## Alternatives

Basic authentication on the media socket would break clients that use the reference's bearer URI.
An independent socket registry would duplicate channel lifetime decisions and introduce cancellation races.

## Consequences

Terminal cleanup removes the connection mapping.
Normal logs must omit connection identifiers and media payloads.
Reconsider the boundary if media ownership moves outside channel lifetime.

## Confirmation

Tests cover single claims, rejected Origins, failed upgrades, cancellation, identifier reuse, and shutdown.
Reference comparisons check incoming media connections and received audio.
The record stays proposed pending integration and review.
