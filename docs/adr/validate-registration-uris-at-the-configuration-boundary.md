---
date: 2026-11-16
status: proposed
---

# Validate registration URIs at the configuration boundary

## Context

[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-10.2) distinguishes registrar targets from addresses of record.
Registration targets influence name resolution, certificate identity, and signaling headers.
Raw strings don't establish those constraints.

## Decision

Use the locked [rsipstack parser](https://github.com/restsend/rsipstack/blob/ca99be1e736f8f6fed3e2f1e48de12384dac9733/src/sip/uri.rs) in the configuration crate.
Construct bounded private URI values with distinct registrar and address roles while source positions remain available.
Keep original URI octets for request serialization and digest authentication.
Reject passwords, unsupported headers, routing parameters, and transport conflicts before network setup.
Validate references using the completed configuration.
Revalidate prepared replacement settings before publication.
Keep parsing free of network and filesystem effects.

## Alternatives

Runtime validation with preserved source spans can provide diagnostics, but carries unchecked inputs farther into effect planning.
A handwritten parser avoids the dependency while duplicating protocol grammar.

## Consequences

The configuration crate gains an existing locked dependency.
Programmatic construction needs checked URI constructors.

## Confirmation

Test malformed inputs, source positions, original octets, transport mismatches, and failed replacements before any connection begins.
Reconsider the parser boundary if its accepted grammar conflicts with the reference.
