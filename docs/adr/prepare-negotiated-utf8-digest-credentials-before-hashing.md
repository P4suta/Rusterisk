---
date: 2026-11-16
status: proposed
---

# Prepare negotiated Unicode digest credentials before hashing

## Context

[RFC 7616](https://www.rfc-editor.org/rfc/rfc7616.html#section-4) requires normalized UTF-8 credentials when a challenge selects UTF-8.
[RFC 8760](https://www.rfc-editor.org/rfc/rfc8760.html#section-2.6) applies the rules to Session Initiation Protocol (SIP).
The client rejects these challenges.

## Decision

Represent absent encoding and negotiated UTF-8 explicitly.
Preserve existing credential octets when encoding is absent.
For UTF-8, enforce UsernameCasePreserved and OpaqueString through pinned precis-profiles 0.2.0.
[RFC 8265](https://www.rfc-editor.org/rfc/rfc8265.html) replaces the referenced profiles without changing their rules.
Preserve username case and space-separated userparts.
Check byte limits before and after preparation and require stable output within four applications.
Use the prepared username and password for A1 and username hashing.
Keep realm, Request-URI, nonce, and entity octets unchanged.
Carry the selected encoding through later authenticated requests.

## Alternatives

Accepting UTF-8 without preparation hashes canonically equivalent credentials differently.
Applying preparation to every challenge changes authentication without negotiation.
Implementing Unicode tables locally duplicates the profile library.

## Consequences

The SIP adapter gains bundled Unicode inputs.
The dependency-free core remains unchanged.
Reconsider when Unicode updates require migration.

## Confirmation

Require independent digest vectors, canonical equivalence, case distinction, width and space mapping, prohibited code points, length boundaries, and authenticated refreshes.
Keep proposed while inbound negotiation and username hashing remain incomplete.
