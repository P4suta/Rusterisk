---
date: 2026-11-16
status: proposed
---

# Resolve digest usernames before verifying responses

## Context

[RFC 7616](https://www.rfc-editor.org/rfc/rfc7616.html#section-3.4) distinguishes plain, extended, and hashed usernames.
The Session Initiation Protocol adopts the digest rules through [RFC 8760](https://www.rfc-editor.org/rfc/rfc8760.html#section-2.6).
Treating a hash as the credential username changes the response calculation.

## Decision

Represent received identities with an exhaustive enum.
Decode extended usernames once with strict byte, text, and length validation.
Resolve configured credentials using the negotiated encoding and selected algorithm.
Calculate the response with prepared credentials before hashing the transmitted username.
Preserve each authorization scope's encoding and username form during renewal.
Never infer credentials from displayed caller identification.

## Alternatives

Rejecting hashed and extended identities preserves the current implementation but excludes negotiated authentication.
Replacing the credential username with its wire representation computes the wrong digest.

## Consequences

Bounded credential lookup gains algorithm work.
Reconsider lookup indexing if configuration limits or measured registration latency require it.

## Confirmation

Require independent hash vectors, normalized credentials, renewal, and rejection tests.
Keep proposed while inbound negotiation remains unverified.
