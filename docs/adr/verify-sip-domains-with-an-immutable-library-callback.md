---
date: 2026-11-16
status: proposed
---

# Verify signaling domains with an immutable library callback

## Context

[RFC 5922](https://www.rfc-editor.org/rfc/rfc5922.html#section-7) gives domain identities in signaling URI certificate extensions precedence over domain-name extensions and prohibits wildcard matching.
The locked encryption library's standard verifier follows different name rules.
Its [custom verification API](https://docs.rs/rustls/0.23.46/rustls/client/danger/struct.DangerousClientConfigBuilder.html#method.with_custom_certificate_verifier) requires a shared callback.

## Decision

Keep only `::tokio_rustls::rustls::RootCertStore` in `SipDomainVerifier`.
Bound certificate bytes and chain length before parsing.
Delegate certificate paths, validity, key usage, and handshake signatures to official library primitives.
Match the original URI host with complete, case-insensitive identities.
Normalize internationalized names before comparison.
Accept numeric endpoints through library address verification.
Decline the optional CN fallback.
Construct the callback only through the registered `::std::sync::Arc::<crate::encryption::sip_domain::SipDomainVerifier>::new(verifier)` in `sip_client_configuration`.
Keep web clients on their standard verifier.

## Alternatives

The standard verifier rejects valid URI identities and accepts wildcard names.
A filter after the handshake still rejects URI-only identities before local checks.
Changing encryption backends would replace the approved cryptography boundary.

## Consequences

The fixed library API receives a stateless implementation with no shared call state.
The source gate rejects aliases, added fields, extra constructors, and unrelated sharing.

## Confirmation

Independently test identity precedence, fallback, wildcard rejection, paths, expiry, names, and signatures.
