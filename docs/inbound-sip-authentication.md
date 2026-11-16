# Inbound authentication

Endpoint `auth` selects credentials for incoming Session Initiation Protocol (SIP) `REGISTER` and initial `INVITE` requests.
Challenges include only credentials referenced by inbound endpoints.
Unused outbound credentials don't select an inbound realm.

An auth object's `supported_algorithms_uas` accepts one to three distinct algorithms in preference order.
The supported names are `MD5`, `SHA-256`, and `SHA-512-256`.
Matching ignores case and surrounding whitespace.
Empty lists, duplicate algorithms, session variants, and unknown names fail configuration validation at the option's line.
The default is `MD5`, matching the [Asterisk 22.9.0 default](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip/config_global.c).
The [inbound challenge decision](adr/bind-inbound-digest-challenges-to-configured-realms-and-algorithms.md) records the compatibility and browser constraints.

Each challenge offers `auth` and `auth-int`, `charset=UTF-8`, and `userhash=true`.
The engine prepares configured usernames with UsernameCasePreserved and passwords with OpaqueString before hashing.
Plain usernames, hashed usernames, and supported `username*` encodings resolve to the configured credentials.
The response calculation uses the actual method, Digest URI, and entity bytes.
Displayed caller identification doesn't select credentials or the dialplan context.
Authentication fails when distinct auth objects resolve to the same prepared identity within one realm.

Each issued nonce binds its realm, algorithm, peer address and port, Call-ID, expiry, and replay counter.
Nonces expire exactly sixty seconds after the engine issues the challenge.
Counters advance only after credentials and endpoint identification succeed.
Failed authentication preserves another outstanding valid nonce.
The engine bounds nonce state at 128 entries and checks the complete challenge response before publishing new nonce state.

Authenticated initial INVITEs reject unsupported `Require` option tags with `420` and an `Unsupported` field.
Malformed option tags receive `400`.
An oversize unsupported-option response receives a bounded `513` response.
Rejected transactions preserve unrelated calls and use the normal cached-response and acknowledgment paths.
