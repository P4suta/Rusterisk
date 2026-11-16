---
date: 2026-11-16
status: proposed
---

# Restrict unversioned dependencies to client development paths

## Context

[Cargo](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#development-dependencies) omits development dependencies without registry versions.
Keeping versions publishes workspace test requirements.
[cargo-deny](https://embarkstudios.github.io/cargo-deny/checks/bans/cfg.html#allow-wildcard-paths) otherwise rejects path-only declarations.

## Decision

Keep registry wildcards denied.
Enable `allow-wildcard-paths` together with a project manifest gate.
The option also permits private normal path/git dependencies and public or private development paths.
Project policy narrows permission to the three direct `rusterisk-client` development paths for server, config, and media.
Require versions for every other path/git dependency.
Inspect normal, build, development, target, and workspace tables.
Resolve workspace inheritance.
Parse version requirements with [semver 1.0.28](https://docs.rs/semver/1.0.28/semver/struct.VersionReq.html).
Cover every member and the independent fuzz, assurance, and proof manifests.
Run the gate before fast checks and security effects.

## Alternatives

Keeping versions preserves unwanted published test requirements.
Moving fixtures into a private crate avoids the exception but changes workspace and example structure.

## Consequences

The permission for three paths needs maintenance.
semver's unsafe storage stays inside the dependency.
Reconsider it when integration fixtures move into an independent crate.

## Confirmation

Reject aliases, other paths, target-specific exceptions, wildcard versions, and unversioned inherited or private dependencies.
Verify both maintained gates reject before external commands.
Run Cargo dependency audits.
