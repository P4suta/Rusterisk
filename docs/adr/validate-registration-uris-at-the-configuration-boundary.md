---
date: 2026-11-16
status: proposed
---

# Validate registration URIs at the configuration boundary

## Context

[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-10.2) distinguishes registrar targets from addresses of record.
Registration targets influence resolution, certificate identity, and headers.
The parser accepts some malformed boundaries.

## Decision

Use the locked [rsipstack parser](https://github.com/restsend/rsipstack/blob/ca99be1e736f8f6fed3e2f1e48de12384dac9733/src/sip/uri.rs) in the configuration crate.
Construct private-field `RegistrarUri` and `RegistrationAddress` values within 2048 ASCII bytes.
Preserve original octets for serialization and digest authentication.
Supplement parser results with bounded checks for host boundaries, escapes, and duplicate parameters.
Reject passwords, headers, routing parameters, and transport conflicts before effects.
Admit User Datagram Protocol (UDP), TCP, and TLS registrars with parser-supported extension parameters.
Apply the component roles in [Table 1](https://www.rfc-editor.org/rfc/rfc3261.html#section-19.1.2).
Resolve references after complete parsing while positions remain available.
Revalidate replacements before publication.
Keep parsing free of effects.

## Alternatives

Runtime validation preserves diagnostics but carries unchecked inputs into effect planning.
A complete handwritten parser avoids the dependency but duplicates grammar.

## Consequences

Configuration gains an existing locked dependency and checked constructors.

## Confirmation

Test malformed inputs, positions, octets, and transport mismatches.
Keep the decision proposed until registration and replacement effects use checked values and reference tests cover behavior.
