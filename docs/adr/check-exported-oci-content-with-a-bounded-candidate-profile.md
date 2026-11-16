---
date: 2026-11-16
status: proposed
---

# Check exported OCI content with a bounded candidate profile

## Context

[OCI layouts](https://github.com/opencontainers/image-spec/blob/v1.1.1/image-layout.md) permit extra files, external blobs, and unknown media types.
Candidate verification needs a complete local export with known execution settings.

## Decision

Validate the maintained export profile.
Check descriptor SHA-256 digests, sizes, and referenced blob membership.
Reject embedded data and external blob locations in the local export profile.
Check architecture variants in both descriptors and image configuration.
Accept known directory headers and bounded nested indexes.
Hash opaque base layers and their uncompressed DiffIDs.
Require the final app layer to match the candidate's nine files, numeric ownership, modes, and empty data directory.
Require one gzip member per layer.
Recognize [BuildKit attestation descriptors](https://docs.docker.com/build/metadata/attestations/attestation-storage/) and their subject references.
Bound archive bytes, entries, JSON, graph depth, and total decompression.
Keep the SDK archive profile unchanged.

## Alternatives

Loading first delegates content inspection to the container engine.
Rejecting every directory or attestation excludes normal exports.

## Consequences

Content consistency doesn't establish the pinned base's identity or authenticate provenance statements.
Those checks and actual startup remain separate.

## Confirmation

Reject changed bytes, metadata, descriptors, unsafe entries, duplicates, missing references, trailing data, and exceeded limits.
Keep proposed until actual exports and runtime checks pass.
