---
date: 2026-11-17
status: proposed
---

# Check browser origins before WebSocket admission

## Context

The approved plan requires Origin validation.
[RFC 6455 §10.2](https://www.rfc-editor.org/rfc/rfc6455.html#section-10.2) recommends checking expected browser origins and rejecting unacceptable handshakes with 403.
[Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_ari.c) uses `allowed_origins` to select cross-origin response headers without rejecting event WebSocket upgrades.

## Decision

Adopt handshake rejection as project policy.
Match a supplied Origin exactly to the configured allowlist before admission.
Reject mismatches with 403.
Keep authentication independent because native clients can omit or forge Origin.
Preserve connections without an Origin header for native clients.
Configure browser clients' exact origins when migrating from Asterisk.

## Alternatives

Response headers alone preserve Asterisk behavior but can't enforce the selected WebSocket admission policy.
Wildcard acceptance admits unrelated browser origins.

## Consequences

Some upgrades accepted by Asterisk fail under Rusterisk.
Apply replacement policy to new admissions while preserving established sessions.
Reconsider when deployments need delegated origins.

## Confirmation

Compare allowed, mismatched, malformed, and absent Origin cases with valid and invalid credentials.
Require admission failure before allocating channel or media resources.
Keep proposed pending comparison.
