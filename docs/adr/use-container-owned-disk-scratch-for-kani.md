---
date: 2026-11-19
status: proposed
---

# Use container-owned disk scratch for Kani

## Context

Four settings harnesses timed out and terminal release exhausted memory under the [bounded gate](verify-call-ownership-in-bounded-kani-containers.md).

## Decision

Keep all twenty-two harnesses, source bytes, assertions, cover goals, unwinding bounds, tool versions, and execution limits.
Select bundled [Kissat 4.0.1](https://github.com/model-checking/kani/blob/kani-0.68.0/kani-dependencies) instead of the default CaDiCaL.
Use a container-specific [anonymous volume](https://docs.docker.com/engine/storage/volumes/#remove-anonymous-volumes) for `/tmp` while preserving the read-only root and source mount.
Record the solver and scratch mount in retained metadata and check every native harness's resolved solver.
Require normal automatic removal or interruption cleanup with anonymous-volume removal before publication.

## Alternatives

Keeping tmpfs preserves the memory pressure.
A host bind can leave root-owned nested files that the producer can't remove.
Separate harness reports would require aggregation and custody rules beyond the existing single native export.

## Consequences

Temporary files consume Docker storage instead of tmpfs capacity.
Keep the existing per-file limit and reject incomplete results.
Reconsider the execution profile if unchanged harnesses still exceed the agreed resources.

## Confirmation

Adapter fixtures must reject a different solver, missing native configuration, altered scratch metadata, and incomplete harness results.
Native completion of all twenty-two harnesses within 2 GiB remains required.
