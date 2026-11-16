---
date: 2026-11-16
status: proposed
---

# Keep transfer progress independent of call teardown

## Context

Accepting a [`REFER` request](https://www.rfc-editor.org/rfc/rfc3515.html#section-2.4.4) doesn't confirm transfer or received audio.
[RFC 5589](https://www.rfc-editor.org/rfc/rfc5589.html#section-5) separates transfer from dialog termination.

## Decision

Keep bounded transfer state in the pure core, keyed by participant generations.
Controller resolves targets within captured endpoint context or checked `TRANSFER_CONTEXT`, matching [Asterisk](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip_refer.c#L1575).
Use saved endpoint authority for permission checks.
Detach retired Dial ownership without ending the surviving call.
Adapters keep bounded notification ownership across `BYE` and distinguish transfer outcomes from received audio.

## Alternatives

An orchestrator in signaling reuses dialogs but duplicates Controller cleanup.
Keeping 501 avoids partial behavior but leaves transfer unimplemented.

## Consequences

Notification ownership may outlive the transferor's `INVITE` usage.
Rusterisk sends early `BYE`.
Asterisk defers termination.
Reconsider when transfer topologies exceed the bounded model.

## Confirmation

Exercise transfer signaling, received audio, failure, and shutdown across three loopback peers.
Reject malformed targets, stale generations, and unauthorized context changes.
Keep proposed until termination timing, out-of-dialog `REFER`, and [Replaces authorization](https://www.rfc-editor.org/rfc/rfc3891.html#section-3) match the reference.
