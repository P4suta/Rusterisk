---
date: 2026-11-16
status: proposed
---

# Require dialog replacement authority before joining calls

## Context

[Attended transfer](https://www.rfc-editor.org/rfc/rfc5589.html#section-7.3) preserves the consultation call.
[RFC 3891](https://www.rfc-editor.org/rfc/rfc3891.html#section-3) separates dialog matching from replacement authorization.
[Asterisk](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip_refer.c#L1488) joins local call legs without redialing.

## Decision

Parse a bounded Replaces selector and resolve one live consultation generation.
For local `REFER`, require transfer permission and matching captured authentication profile, realm, username, and context.
Displayed caller identification grants no authority.
Validate two disjoint Dial pairs and their complete bridge membership before mutation.
Let the pure core decide the atomic handoff of four call generations.
Keep notification ownership independent of retired dialogs.
Handle direct `INVITE` replacement and [`sips:` authorization](https://www.rfc-editor.org/rfc/rfc5630.html#section-5.1.1.3) in dedicated units, rejecting unsupported requests.

## Alternatives

Blind redial reuses cleanup but discards consultation ownership.
Dialog identifiers alone can't establish authorization.

## Consequences

Local policy excludes delegated and cross-profile replacement.
Reconsider when supporting delegated replacement or larger bridge topologies.

## Confirmation

Compare existing-dialog audio and notifications with Asterisk.
Reject unauthorized, ambiguous, stale, and overlapping participants without changing calls.
Verify handoff and terminal cleanup through properties, proofs, Miri, and mutation testing.
Keep proposed until reference and authorization tests pass.
