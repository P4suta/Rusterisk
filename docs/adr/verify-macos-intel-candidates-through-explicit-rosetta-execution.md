---
date: 2026-11-16
status: proposed
---

# Verify macOS Intel candidates through explicit Rosetta execution

## Context

The local Mac uses Apple silicon, while distributions include macOS Intel executables.
[Apple supports executing and debugging Intel binaries through Rosetta](https://developer.apple.com/documentation/apple-silicon/building-a-universal-macos-binary).

## Decision

Support an explicit macOS Intel target on an Apple silicon Mac.
Report translated execution explicitly.
Verify the candidate's architecture, bytes, version, configuration, startup, and shutdown through the maintained runtime checks.
Fail when translation is unavailable without installing system components.
Keep other operating-system and architecture mismatches rejected.

## Alternatives

A dedicated Intel Mac provides native CPU observations and remains the strongest release check.
Cross-compilation alone doesn't verify executable startup.

## Consequences

Translated execution doesn't establish native Intel performance or unsupported CPU instructions.
Apple's [translation limits](https://developer.apple.com/documentation/apple-silicon/about-the-rosetta-translation-environment) apply.
Reconsider the adapter when Rosetta support changes.

## Confirmation

Reject implicit translation and unsupported host combinations.
Require actual candidate startup and listener cleanup under the reported execution mode.
Keep proposed until you accept the reviewed choice.
