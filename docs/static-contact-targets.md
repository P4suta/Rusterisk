# Static contact targets

The `contact` option in a `type=aor` section supplies a static Session Initiation Protocol (SIP) target.
Configuration validates the target within 2048 ASCII bytes and preserves its original octets.
Malformed hosts, escapes, duplicate parameters, passwords, URI headers, and unsupported routing components fail at the `contact` option line.
The profile follows the URI component roles in [RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-19.1.2).

Each endpoint that references the address must use a compatible transport.
References may appear before their declarations.
The `sips` scheme requires TLS or secure WebSockets.
An explicit URI transport uses `udp`, `tcp`, or `ws` according to the configured transport.
TLS uses the URI parameter `transport=tcp`.
Secure WebSockets use `transport=ws`, as defined by [RFC 7118](https://www.rfc-editor.org/rfc/rfc7118.html#section-5.2).
The profile rejects `transport=tls` before generation, as required by [RFC 5630](https://www.rfc-editor.org/rfc/rfc5630.html#section-5.1.1).
An unused address still receives URI validation.

The SIP engine also checks outgoing targets after callers mutate public configuration fields.
Legacy Contact receive parsing remains separate from target generation.
