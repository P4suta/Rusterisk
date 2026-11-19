---
date: 2026-11-15
status: proposed
---

# Bound encrypted connection lifetimes

## Context

The approved plan requires AWS-LC encryption and bounded resources without shared call state.

## Decision

Use axum-server 0.9.2's concrete `RustlsAcceptor` in an owned connection loop.
Limit admission to 64 connections, handshakes to five seconds, and shutdown to five seconds.
Bound certificate and private-key reads.
Track upgraded event callbacks with owned lifetime channels and wait for their release during shutdown.
Confine the library's shared configuration to encryption settings.

## Alternatives

Direct tokio-rustls needs an explicit shared `ServerConfig` at its fixed boundary.
The selected helper adds a dependency while keeping that boundary inside reviewed library code.

## Consequences

Rusterisk supplies admission and task cleanup rather than using the library's unlimited accept loop.
Reconsider the helper if a fixed API offers owned configuration directly.

## Confirmation

[The pinned acceptor source](https://github.com/programatik29/axum-server/blob/ad890d8b4f168cca6fb44c0ee20201f4f4178d08/src/tls_rustls/mod.rs) exposes a concrete acceptor and configurable handshake timeout.
Encrypted requests, malformed credentials, connection saturation, and shutdown must pass before acceptance.
