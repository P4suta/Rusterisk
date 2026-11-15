---
date: 2026-11-15
status: proposed
---

# Compare reference audio through container addresses

## Context

Received audio comparisons need datagram access to the disposable reference.
[Docker](https://docs.docker.com/engine/network/#published-ports) and [OrbStack](https://docs.orbstack.dev/docker/network#direct-container-access) document direct container access from the host.
The existing runner publishes management traffic on loopback.

## Decision

Inspect only the runner's container for one private IPv4 address.
Pass the address to the bounded comparison process.
Choose the host's return address through an operating system route probe.
Keep the reference's existing processor, memory, port range, and lifetime limits.

## Alternatives

Host networking would simplify addressing but share listener ports.
Publishing every datagram port would require translating advertised addresses and preserving port pairs.

## Consequences

The integration stage requires direct container routing.
A host without direct routing fails the comparison explicitly.
Reconsider the topology when a required integration host lacks direct routing.

## Confirmation

Parser tests reject public addresses and ambiguous inspection output.
Audio tests compare known samples received in both directions through each server.
The record stays proposed pending cross-platform integration.
