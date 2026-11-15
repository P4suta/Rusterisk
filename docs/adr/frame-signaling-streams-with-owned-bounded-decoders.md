---
date: 2026-11-15
status: proposed
---

# Frame signaling streams with owned bounded decoders

## Context

[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-7.5) requires stream message boundaries from `Content-Length` and excludes leading `CRLF` pairs from messages.
The existing engine accepts complete packets with a 16 KiB limit.

## Decision

Keep partial bytes in one connection-owned synchronous decoder.
Return one owned message or leading `CRLF` pair with the consumed input count.
Check the complete declared size before reading its body.
Make malformed framing and connection closure terminal.
Keep [keepalive meaning and replies](https://www.rfc-editor.org/rfc/rfc5626.html#section-4.4.1) in the connection adapter.

## Alternatives

The [dependency's stream codec](https://docs.rs/crate/rsipstack/0.8.5/source/src/transport/stream.rs) needs its broader transport feature and uses a different size limit.
Reading headers and bodies directly inside a selection loop risks losing partial input after cancellation.

## Consequences

Each connection holds one bounded buffer.
Reconsider the boundary if the engine's packet limit changes.

## Confirmation

Tests cover every byte boundary, combined messages, binary bodies, keepalive pairs, malformed lengths, size limits, cancellation, and terminal errors.
Transport integration remains pending.
