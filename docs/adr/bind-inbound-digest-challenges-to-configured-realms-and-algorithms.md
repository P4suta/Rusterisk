---
date: 2026-11-17
status: proposed
---

# Bind inbound digest challenges to configured realms and algorithms

## Context

[Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip/config_global.c#L55) defaults inbound authentication to MD5.
Its [authentication option](https://docs.asterisk.org/Asterisk_22_Documentation/API_Documentation/Module_Configuration/res_pjsip/#supported_algorithms_uas) orders algorithm challenges.
The pinned [browser client](https://github.com/onsip/SIP.js/blob/0.21.2/src/core/messages/digest-authentication.ts#L68) supports MD5 only.

## Decision

Parse `supported_algorithms_uas` as a nonempty ordered set of MD5, SHA-256, and SHA-512-256.
Preserve the reference default and reject unknown or duplicate entries.
Generate challenges from inbound endpoint credentials.
Bind each issued nonce to its realm, algorithms, peer, call, expiry, and replay counter.
Advertise UTF-8 and username hashing.
Verify prepared credentials with the issued profile, independently of displayed caller identification.
Reject ambiguous normalized identities.

## Alternatives

Always preferring SHA-256 breaks the pinned browser client.
Accepting algorithms absent from the challenge weakens configured policy.

## Consequences

MD5 remains a compatibility default despite its cryptographic weakness.
Configure SHA-256 where clients support it.
Reconsider the default when the browser supports stronger algorithms.
Bound challenge headers before publishing nonce state.

## Confirmation

Require configuration rejection, ordered challenges, Unicode identities, hashed lookup, replay, and cross-profile refusal tests.
