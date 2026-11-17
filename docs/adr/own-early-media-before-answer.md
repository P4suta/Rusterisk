---
date: 2026-11-17
status: proposed
---

# Own early media before answer

## Context

[Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/res/stasis/control.c#L763-L784) requests PROGRESS before starting music on unanswered channels.
The [channel driver](https://github.com/asterisk/asterisk/blob/22.9.0/channels/chan_pjsip.c#L1577-L1583) maps PROGRESS to 183.
[RFC 3261 §13.2.1](https://www.rfc-editor.org/rfc/rfc3261.html#section-13.2.1) permits provisional Session Description Protocol (SDP) matching the final answer.

## Decision

Admit early music through an explicit core phase.
Keep ordinary Waiting closed.
The call stays Ringing until Answer.
Bind provisional SDP, one negotiated transport, and music to the channel generation.
Prepare transport before publishing 183 and start music after admission.
Reuse the transport, address, and exact SDP in 200.
Preserve `channels.mute` and negotiated direction.
Release sources, reservations, and sockets on CANCEL, failure, shutdown, or stale attachment.

## Alternatives

Opening Waiting would permit unprepared media.
Implicit Answer would change call acceptance.
Reliable provisional responses require a separate `PRACK` contract.

## Consequences

Extend bounded pending-dialog state and response caching without duplicating transport ownership.
Support inbound INVITE offers first.
Reconsider for outgoing early media or reliable provisional negotiation.

## Confirmation

Require 204, 183 with SDP, and known received audio before 200.
Compare retransmissions, Answer reuse, `channels.mute`, cancellation races, and port release with Asterisk.
Keep proposed pending review and tests.
