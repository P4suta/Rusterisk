---
date: 2026-11-16
status: proposed
---

# Authenticate signaling requests with validated digest inputs

## Context

[RFC 8760](https://www.rfc-editor.org/rfc/rfc8760.html#section-2) updates Session Initiation Protocol (SIP) authentication through [RFC 7616](https://www.rfc-editor.org/rfc/rfc7616.html).
The locked [digest helper](https://github.com/restsend/rsipstack/blob/ca99be1e736f8f6fed3e2f1e48de12384dac9733/src/sip/services.rs) uses an MD5 body hash for integrity calculations across algorithms.

## Decision

Validate URI, realm, nonce, algorithm, protection, and counters before hashing.
Bound text and use concrete hash primitives.
Support MD5, SHA-256, and SHA-512-256.
Use the selected algorithm for integrity body hashes, including empty bodies.
Treat session derivation explicitly.
Distinguish unsupported algorithms from absent algorithms.
Select supported offers in server order using the configured realm.
Separate authentication identity from caller identification.
Keep unspecified realms for role-dependent evaluation.
Use [Asterisk's](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip/config_global.c) inbound default and an outbound wildcard.
Share registration and call authentication.
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
