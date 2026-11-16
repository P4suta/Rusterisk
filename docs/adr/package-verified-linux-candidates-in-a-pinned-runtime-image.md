---
date: 2026-11-16
status: proposed
---

# Package verified Linux candidates in a pinned runtime image

## Context

Recompiling inside a runtime image disconnects its executable from the verified server archive.
The distribution contract requires Linux OCI images and startup checks.

## Decision

Copy verified Linux candidate bytes into a digest-pinned Debian runtime.
Use numeric [`USER` and exec-form `ENTRYPOINT`](https://docs.docker.com/reference/dockerfile/) declarations.
Keep configuration and data under separate directories.
Preserve the loopback configuration examples and both license texts.
Export an [OCI image layout](https://docs.docker.com/build/exporters/oci-docker/) and verify its descriptor hashes, platform, app files, user, command, and source identity.
Require actual startup and client requests from the exported bytes.
Bound builder and runtime resources and remove owned containers, builders, and temporary files.

## Alternatives

A fresh image build duplicates compilation and requires another executable comparison.
A scratch image requires an independently maintained runtime library closure.

## Consequences

The base image supplies system libraries and their license files.
Native dependency and startup checks remain necessary.
Publication remains separate.

## Confirmation

Reject changed payloads, descriptors, platforms, execution configuration, and cleanup failures.
Keep the decision proposed until exported candidates pass actual startup and HTTP verification.
