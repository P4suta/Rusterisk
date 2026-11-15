---
date: 2026-11-15
status: proposed
---

# Compare reference audio through container addresses

## Context

Received audio comparisons need access to the disposable reference.
[Docker](https://docs.docker.com/engine/network/#published-ports) and [OrbStack](https://docs.orbstack.dev/docker/network#direct-container-access) document direct container access from the host.
The existing runner publishes management traffic on loopback.

## Decision

Inspect only the runner's container for one private IPv4 address.
Pass the address to the bounded comparison process.
Choose datagram return addresses through an operating system route probe.
Resolve `host.docker.internal` inside the reference for stream callbacks.
Keep the reference's existing processor, memory, port range, and lifetime limits.

## Alternatives

Host networking would simplify addressing but share listener ports.
Publishing every datagram port would require translating advertised addresses and preserving port pairs.

## Consequences

The integration stage requires direct datagram routing and a working host callback.
Reconsider the topology when a required integration host lacks direct routing.

## Confirmation

Parser tests reject ambiguous addresses and invalid callbacks.
Audio tests compare known samples received in both directions through each server.
The record stays proposed pending cross-platform integration.
