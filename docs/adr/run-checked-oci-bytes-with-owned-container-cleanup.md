---
date: 2026-11-16
status: proposed
---

# Run checked OCI bytes with owned container cleanup

## Context

[Docker load](https://docs.docker.com/reference/cli/docker/image/load/) restores image names and content.

## Decision

Privately copy and validate the candidate's exact archive before loading.
Reject name annotations and inspect the loaded configuration digest.
Run the image ID with unique names and an ownership label.
Refuse existing names without deleting them.
Check version and packaged/runtime configuration before startup.
Use host networking for loopback-only HTTP and Session Initiation Protocol (SIP) fixtures on Linux and [OrbStack](https://docs.orbstack.dev/docker/network).
Build the independent SDK consumer before startup.
Require readiness, typed HTTP operations, explicit [Docker stop](https://docs.docker.com/reference/cli/docker/container/stop/), and the foreground client's zero exit.
Restart on the same ports to check listener release.
Pin stop and removal to the inspected container ID after checking image and label.
Keep imported image cache.
Combine primary, container, and private-directory cleanup failures.

## Alternatives

Mutable tags can select different bytes.
Signal forwarding alone doesn't establish successful shutdown.
Bridge networking adds unrelated publication and routing requirements.

## Consequences

Runtime checks don't authenticate provenance or establish the base image's identity.
Engine availability remains a release-stage prerequisite.

## Confirmation

Reject identity changes, existing names, missing readiness, failed shutdown, and foreign cleanup targets.
Keep proposed until the exported candidate passes SDK and restart checks.
