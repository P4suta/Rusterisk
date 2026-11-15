---
date: 2026-11-15
status: proposed
---

# Build the reference with a dedicated bounded builder

## Context

Compatibility requires the exact Asterisk 22.9.0 reference.
The development machine also runs unrelated containers.
[Docker's container driver](https://docs.docker.com/build/builders/drivers/docker-container/) supports dedicated memory and processor quotas.

## Decision

Build the checksum-verified reference source with a dedicated, digest-pinned BuildKit instance.
Limit the builder to two processors and 2 GiB without swap.
Use two compiler jobs and a 30-minute build deadline.
Remove the builder after success, failure, or timeout.
Keep the resulting reference image local.
Print scope and limits before execution through a dry run.

## Alternatives

A shared builder can consume resources outside the experiment's budget.
A host installation changes the development environment.
An unverified image can contain another reference version.

## Consequences

Cold builds require network downloads and temporary storage.
Timeouts remain failed verification.
Reconsider the budget after measuring a cold build.

## Confirmation

Test process success, failure, timeout, and child reaping.
Check container limits, reference version, and cleanup during a local rehearsal.
Keep the record proposed until the rehearsal passes.
