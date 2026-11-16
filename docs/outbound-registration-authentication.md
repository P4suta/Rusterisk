# Outbound registration authentication

`RegistrationResponse::authenticated_retry` consumes a matched challenge receipt and returns at most one new transaction.
`authenticate` returns either `RegistrationAuthentication::Retry` or the owned receipt in `RegistrationAuthentication::Rejected`.
The existing convenience method returns `None` and drops rejected receipts.
The caller supplies a later sequence under the same boot identifier, a validated client nonce and nonzero nonce count, and a monotonic timestamp.
The generation manager remains responsible for global request ordering and authentication session counters.

The shared Digest implementation selects the first supported offer matching configured credentials, following [RFC 8760](https://www.rfc-editor.org/rfc/rfc8760.html#section-2.4).
It hashes the serialized Request-URI and request body.
Missing quality offers select explicit `auth`, and offered `auth-int` uses the selected algorithm's entity hash.
Algorithm selection stays within the received challenge list.
The current profile rejects `charset` and `userhash=true` offers.

Origin and proxy authentication keep independent headers.
A retry renews the other scope's authorization with its existing client nonce and an increased count, following [RFC 7616](https://www.rfc-editor.org/rfc/rfc7616.html#section-3.4).
Another challenge for the same authorization scope requires a changed server nonce with `stale=true`.
Each attempt chain permits at most four authentication retries.

Matched successful receipts support [Contact grant interpretation](outbound-registration-grants.md).
Native registration authentication and refresh use the [startup driver](outbound-registration-startup.md).
Delayed failure scheduling and configuration replacement remain pending.
The [generation decision](adr/own-outbound-registrations-by-configuration-generation.md) describes their required ownership.
