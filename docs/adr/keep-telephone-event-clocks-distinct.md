---
date: 2026-11-15
status: proposed
---

# Keep telephone event clocks distinct

## Context

Opus uses a 48 kHz clock while a peer may offer telephone events at 8 kHz.
[The sender rules](https://datatracker.ietf.org/doc/html/rfc7160#section-4.1) require different synchronization sources for different clock rates when the sender publishes control reports.

## Decision

Share packet sequence numbers when clocks match.
Use distinct synchronization sources when clocks differ and maintain independent timestamps and counters.
Keep both sources inside the call's owned transport with bounded event queues.

## Alternatives

Negotiating matching rates avoids another source but depends on peer support.
One source with different rates would violate the selected reporting rules.

## Consequences

Compound control reports describe both clocks with one canonical name.
Use one description packet per source because [the library parser](https://docs.rs/rtcp-types/0.3.0/src/rtcp_types/sdes.rs.html) treats leading zeros in subsequent identifiers as padding.
Shutdown releases both sources.
Reference testing must resolve whether source changes disrupt Asterisk's audio processing.

## Confirmation

Tests must compare received audio and digits with matching and different clock rates, sequence wraparound, lost end packets, and cancellation.
[The telephone event specification](https://datatracker.ietf.org/doc/html/rfc4733#section-2.5.1.4) defines repeated end reports.
Reconsider the source strategy if reference calls lose audio while receiving telephone events.
