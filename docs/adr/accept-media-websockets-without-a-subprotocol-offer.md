---
date: 2026-11-17
status: proposed
---

# Accept media WebSockets without a subprotocol offer

## Context

The home-telephone gateway sends Basic credentials without `Sec-WebSocket-Protocol`.
[Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_http_websocket.c#L794-L805) selects the sole registered protocol when the offer is absent.
Its [media listener](https://github.com/asterisk/asterisk/blob/22.9.0/channels/chan_websocket.c#L1913-L1924) registers only `media`.

## Decision

Accept an absent protocol offer on `/media/{connectionId}` without returning an unsolicited protocol header.
Require an offered protocol list to contain `media` within the existing 256-byte bound.
Preserve Origin validation and the [single-use URI capability](claim-media-connections-in-the-control-task.md).
Basic credentials remain optional.
Connections still require the capability.

## Alternatives

Requiring an explicit `media` offer preserves current rejection behavior but excludes the gateway's reference-compatible request.
Accepting arbitrary offers would hide unsupported protocols.

## Consequences

Protocol omission changes only negotiation.
Reconsider omission if the listener registers another media protocol.

## Confirmation

Use identical query-only requests with both servers.
Check variable lookup, exact Stasis identity, unsolicited-header absence, received audio in both directions, and bounded cleanup.
Keep explicit negotiation and rejected Origin, protocol, oversize-offer, and duplicate-claim tests.
Keep the record proposed pending runtime comparison and review.
