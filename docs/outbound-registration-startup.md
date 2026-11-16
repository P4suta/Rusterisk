# Outbound registration startup

Signaling startup connects numeric-address and hostname User Datagram Protocol (UDP) registration targets to the existing listener.
Each request advertises the listener's reachable address and preserves the configured Request-URI octets.
The starting configuration owns at most sixteen clients.
Configurations without registrations start no registration work.

Initial requests follow the [Asterisk 22.9.0 delay](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip_outbound_registration.c#L2005).
`max_random_initial_delay=0` still waits one second.
Positive values select an integer delay from one through `max_random_initial_delay` seconds.
Round-robin polling emits at most one packet per signaling tick.
Requests to the same physical registrar wait for the preceding transaction's final response or settlement deadline.
The existing bounded transaction supplies retransmission and a 32-second timeout.
Shutdown cancels waiting and transmitted clients before releasing the listener.

Hostname lookups use the shared bounded resolver and actor-owned cache.
Calls and registration lookups share sixteen setup slots.
Cancelled jobs keep their slots until reaped.
Each lookup owns a configuration lease and a generation key.
Released leases or mismatched keys prevent cache updates and registration transmission.
Signaling continues processing packets while lookups wait.
Shutdown closes the configuration lifetime and joins its lookup jobs.

TCP registrations with numeric addresses use owned outbound connections.
Authentication and refresh keep the same connection generation.
Preparation distinguishes call lifetimes from configuration lifetimes.
Configuration cancellation prevents prepared flows from entering the registration driver.
Disconnection stops refresh on the closed flow.
Accepted streams require a complete first message within five seconds.
Initiated streams leave response waits to their transaction.
Both roles keep five-second assembly deadlines for partial messages.
More bytes don't extend an assembly deadline.

TLS registrations verify certificate chains and peer names through the shared encrypted transport.
Service lookups preserve the original registrar domain for certificate checks and the TLS server name.
URI-only domain certificates follow [RFC 5922](https://www.rfc-editor.org/rfc/rfc5922.html#section-7.3).
Untrusted or mismatched peers receive no registration request.
Authentication and refresh reuse the verified connection.
TLS uses port 5061 when the registrar URI omits a port.
Initial TCP and TLS requests check the first usable resolved candidate for an open matching flow before connecting.
TLS reuse requires the captured client configuration, settings generation, and verified domain.
Setup metadata remains limited to sixteen slots while sixty-four streams are active or connecting.
An open backup doesn't bypass a preferred candidate.
After an earlier connection attempt fails, setup checks the next usable candidate for a matching open flow before connecting.
The driver uses matching provisional and final responses, the shared Digest implementation, and validated Contact grants.
Successful grants schedule refresh requests with renewed authorization counters.
423 responses can raise requested `Expires` through one positive `Min-Expires` value.
`Min-Expires` must exceed the transmitted value.
Corrections preserve the route, Contact, Call-ID, URI octets, and authentication scopes.
Corrections advance CSeq and Digest counters.
The driver keeps the learned value for refreshes without changing the starting settings.
Malformed values, removal requests, and cancellation don't create corrections.

Failed final responses and timeouts follow the [checked retry policy](adr/schedule-configured-registration-retries-after-checked-failures.md).
Positive Retry-After values take precedence without consuming configured retry budget.
Configured waits use fixed intervals and stop at `max_retries`.
Successful grants reset the retry counter.
Waiting attempts assign CSeq only when due and let another address of record progress.
Cancellation drops pending retries without closing a shared flow.
Retries from failed responses renew existing authorization on an open TCP or TLS connection.
A closed flow preserves the retry deadline and resolves again when due.
Old connection generations can't stop the replacement registration.
Alternate servers, shared 503 suppression, and configuration replacement remain pending.
The [pure transaction APIs](outbound-registration-transactions.md) and [generation decision](adr/own-outbound-registrations-by-configuration-generation.md) define the remaining integration boundaries.
