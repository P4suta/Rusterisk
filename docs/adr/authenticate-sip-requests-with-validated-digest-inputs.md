---
date: 2026-11-16
status: proposed
---

# Authenticate signaling requests with validated digest inputs

## Context

[RFC 8760](https://www.rfc-editor.org/rfc/rfc8760.html#section-2) updates Session Initiation Protocol (SIP) authentication through [RFC 7616](https://www.rfc-editor.org/rfc/rfc7616.html).
The locked [digest helper](https://github.com/restsend/rsipstack/blob/ca99be1e736f8f6fed3e2f1e48de12384dac9733/src/sip/services.rs) uses an MD5 body hash for integrity calculations across algorithms.

## Decision

Validate URI, realm, nonce, algorithm, protection mode, and counters before computing authentication.
Bound text and use concrete hash primitives.
Support MD5 for compatibility and SHA-256 for stronger authentication.
Compute integrity body hashes with the selected algorithm, including empty bodies.
Treat session derivation explicitly.
Keep unsupported algorithms distinct from absent algorithms.
Unsupported challenges never become MD5 authentication.
Select supported offers according to the server's order and configured realm.
Keep authentication identity separate from caller identification.
Share digest calculation and validation across registration and call authentication.
Compare decoded responses in constant time.

## Alternatives

Using the existing helper unchanged produces incorrect integrity responses.
Separate implementations would duplicate the same authentication rules.

## Consequences

RFC 7616 defines SHA-256 as mandatory and SHA-512-256 as a backup algorithm.
Project completion requires applicable recommendations, including SHA-512-256.
Reject unsupported algorithms until parsing and independent vectors cover them.
Handling protection parameters is mandatory.

## Confirmation

Check independent digest vectors, empty and nonempty bodies, session derivation, stale nonces, replay, realm mismatch, and unsupported challenges.
