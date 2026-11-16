---
date: 2026-11-16
status: proposed
---

# Generate Contacts from address security

## Context

[RFC 5630](https://www.rfc-editor.org/rfc/rfc5630.html#section-5.1.1) distinguishes `sip` over TLS from `sips` and prohibits generated `transport=tls` parameters.
[RFC 7118](https://www.rfc-editor.org/rfc/rfc7118.html#section-9.2) requires secure WebSockets for `sips` addresses.
Generators conflate address security and encryption.

## Decision

Represent URI security and physical transport as distinct types.
Keep Via tokens `TLS` and `WSS`, with URI parameters `transport=tcp` and `transport=ws`.
Select `sips` Contacts for `sips` Request-URIs or applicable route fields.
When every registration Contact uses `sips`, From and To must use `sips`.
Registrars require the Request-URI and Path entries to use `sips` for `sips` Contacts, under [Section 5.2](https://www.rfc-editor.org/rfc/rfc5630.html#section-5.2).
Preserve the initiating dialog's security during target refresh.
Reject conflicting roles and unsupported generated targets.
Preserve URI octets and legacy receive parsing.

## Alternatives

Copying legacy parameters preserves fixtures but violates generation requirements.
Inferring `sips` from encryption incorrectly promises security across the entire request path.
Rewriting targets hides intent and changes Digest inputs.

## Consequences

Legacy `transport=tls` settings require migration to configured TLS with appropriate `sip` or `sips` URIs.
Asterisk integration keeps legacy receive parsing.
Rejected settings identify the option location.

## Confirmation

Exercise `sip` and `sips` over TLS and secure WebSockets, registration role conflicts, dialog answers and refresh, legacy receive parsing, and migration diagnostics.
