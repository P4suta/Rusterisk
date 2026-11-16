# Outbound registration updates

`RegistrationResponse::refresh` builds a transaction from a validated active Contact grant.
Its lifetime uses `NonZeroU32` to require a positive interval.
`RegistrationResponse::remove` requests removal with `Expires: 0`, following [RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-10.2.2).
An absent, ambiguous, failed, or removed grant rejects both operations.

The generated request preserves the original Request-URI octets, Contact, Call-ID, From, To, and physical peer.
The caller supplies a later sequence under the same boot identifier and a fresh branch.
The new transaction has a checked 32-second deadline and a fresh four-retry authentication budget.
Both authorization scopes keep their server nonce, client nonce, and opaque value while increasing the nonce count and recomputing the request digest.
Missing matching credentials and exhausted counters reject the update before transmission.

The generation manager remains responsible for unique sequences, the refresh deadline, serialization at each registrar, and the [same network address recommendation](https://www.rfc-editor.org/rfc/rfc3261.html#section-10.2.4).
Native registration and update transmission remain pending.
