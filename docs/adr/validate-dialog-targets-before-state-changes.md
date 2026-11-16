---
date: 2026-11-16
status: proposed
---

# Validate dialog targets before state changes

## Context

[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-12.2) uses received Contact URIs for later requests.
[RFC 5630](https://www.rfc-editor.org/rfc/rfc5630.html#section-5.1.1) prohibits generated `transport=tls` parameters and security downgrades.
Raw targets can reach request generation after partial state changes.

## Decision

Build owned dialog targets from bounded Contact parsing.
Keep received URIs separate from generated roles.
Ignore disallowed dialog Contact components under [Section 19.1.1](https://www.rfc-editor.org/rfc/rfc3261.html#section-19.1.1).
Refuse unavailable capabilities under [Section 19.1.5](https://www.rfc-editor.org/rfc/rfc3261.html#section-19.1.5).
Capture physical transport and preserve the initiating dialog's URI security.
Validate targets before sequence, state, or media changes.
Generate dialog requests through checked targets.

## Alternatives

Checking each send preserves raw fields but repeats validation across methods.
Rewriting peer targets can change route selection and authenticated request octets.
Guessing a previous target for an unusable response risks sending requests to another resource.

## Consequences

Unsupported targets require typed failure and bounded cleanup.
The mandatory `ACK` behavior for an unusable final response target remains unresolved.

## Confirmation

Test forbidden generated parameters, target roles, transport mismatches, security preservation, and rejected-update atomicity.
Keep the decision proposed until review resolves `ACK` handling for final responses and reference tests pass.
