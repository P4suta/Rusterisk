---
date: 2026-11-16
status: proposed
---

# Bind Docker image identities to verified OCI descriptors

## Context

[Moby's classic store](https://raw.githubusercontent.com/moby/moby/8af9fe3/daemon/images/image_inspect.go) exposes configuration IDs.
Its [containerd store](https://raw.githubusercontent.com/moby/moby/8af9fe3/daemon/containerd/image_inspect.go) exposes target descriptor IDs.

## Decision

Read one exact `Loaded image ID:` line.
Accept only the verified configuration or selected image manifest digest.
Inspect that ID once before constructing the execution plan.
Check Linux architecture and ID.
The configuration profile requires no image descriptor.
The manifest profile requires matching descriptor digest, size, and OCI manifest media type.
[Containerd's GetImage](https://raw.githubusercontent.com/moby/moby/8af9fe3/daemon/containerd/image.go) constructs the image with its target digest.
[Container inspection](https://raw.githubusercontent.com/moby/moby/8af9fe3/daemon/inspect.go) returns that stored engine identity.
Match `.Image` and the optional manifest descriptor to the selected profile.
Keep control bound to the inspected container ID and ownership label.

## Alternatives

Trying both digests hides the engine's selected representation.
Accepting arbitrary loaded IDs loses the checked graph boundary.

## Consequences

Imported index IDs are outside these runtime profiles.
Archive validation still accepts bounded indexes and attestations.
Keep attestations enabled.

## Confirmation

Reject unrelated IDs, tags, extra loaded images, platform changes, and descriptor differences.
Keep proposed until the exported candidate passes actual execution.
