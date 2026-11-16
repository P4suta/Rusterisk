---
date: 2026-11-16
status: proposed
---

# Verify distributions from local candidate bytes

## Context

Workspace checks don't establish distribution startup.
[Cargo packaging](https://doc.rust-lang.org/cargo/commands/cargo-package.html) rewrites manifests.

## Decision

Use typed xtask operations to create and verify local server archives, Linux OCI images, and core/API/client packages from immutable source.
Select Linux/macOS x86_64 and ARM64, plus Windows x86_64.
Unix uses tar.gz.
Windows uses zip.
Include example configuration, both license texts, and package documentation.
Keep normal SDK registry versions.
Omit server/config/media dev-dependencies through path-only declarations and exclude workspace-only tests/examples from client packages.
Preserve checkout tests.
Build and run an independent consumer using extracted candidates.
Smoke-test extracted servers and [OCI exports](https://docs.docker.com/build/exporters/oci-docker/) on native hosts with bounded cleanup.
Publication remains separate.

## Alternatives

cargo-dist provides maintained archives and workflows.
Existing xtask ownership integrates independent verification phases more directly.
Source-only builds leave packaging defects unchecked.

## Consequences

Native hosts and runtime dependency inspection add cost.
Reconsider cargo-dist when installer requirements dominate.
The decision remains proposed pending native verification.

## Confirmation

Reject missing licenses/configuration, checkout paths, stale bytes, wrong targets, and cleanup failures.
Require consumer build/run, native startup, and applicable checks through release-check.
Metadata-only probes don't verify execution.
