---
date: 2026-11-16
status: proposed
---

# Require dialog replacement authority before joining calls

## Context

[Asterisk](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip_refer.c#L1488) joins existing legs for local [attended transfer](https://www.rfc-editor.org/rfc/rfc5589.html#section-7.3).
[RFC 3891](https://www.rfc-editor.org/rfc/rfc3891.html#section-3) requires dialog matching and replacement authorization.

## Decision

Resolve a bounded Replaces selector to one live consultation generation.
For local `REFER`, require transfer permission and matching captured authentication profile, realm, username, and context.
Displayed caller identification grants no authority.
Validate two disjoint Dial pairs and complete bridge membership before mutation.
The pure core decides the atomic handoff of four generations.
Keep notification monitors separate from the surviving pair's bridge and generation ownership.
Release the bridge and peer once on a current participant's departure.
Reject direct `INVITE` replacement and [`sips:` authorization](https://www.rfc-editor.org/rfc/rfc5630.html#section-5.1.1.3) until dedicated adapters work.

## Alternatives

Blind redial discards consultation ownership.
`DialplanToken` reuse binds survivors to retired execution.
Dialog identifiers alone don't grant authority.

## Consequences

Local policy excludes delegated and cross-profile replacement.
Pair state persists through teardown.
Reconsider for delegated authority or larger topologies.

## Confirmation

Compare existing-dialog audio, notifications, and peer cleanup with Asterisk.
Reject unauthorized, ambiguous, stale, and overlapping participants without changing calls.
Verify handoff and pair release through properties, proofs, Miri, and mutation testing.
