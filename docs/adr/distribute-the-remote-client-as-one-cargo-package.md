---
date: 2026-11-20
status: proposed
---

# Distribute the remote client as one Cargo package

## Context

An operator needs to check connectivity and reload a running server from another machine.
[Cargo installs executable targets](https://doc.rust-lang.org/cargo/commands/cargo-install.html).
The Rust SDK depends on two workspace libraries.

## Decision

Make `rusterisk-cli` the only workspace package eligible for crates.io.
Give its executable the package name.
Use registry dependencies for authenticated `ping` and `reload`.
Share the reload command with the server executable.
Preserve the [configuration reload contract](reload-startup-settings-through-authenticated-control.md) and the SDK's independent consumer checks.

## Alternatives

A binary linked to the full SDK would require publishing its [normal path dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#local-paths-in-published-crates).
Installing the server binary brings its media and storage dependencies to the operator's machine.

## Consequences

The standalone client implements two HTTP operations.
Reconsider the dependency boundary if operators need the SDK's call and event lifetimes.

## Confirmation

Install the packaged command outside the checkout.
Check the request method, credentials, response, and exit status for both operations.
Check the workspace publication settings with Cargo metadata.
