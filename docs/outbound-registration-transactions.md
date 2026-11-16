# Outbound registration transactions

`RegistrationTransaction` owns one bounded request and accepts monotonically increasing milliseconds from its caller.
`poll` returns at most one owned transmission or timeout event.
Construction starts a 32-second settlement deadline.
User Datagram Protocol (UDP) retries follow the 500, 1000, 2000, and capped 4000 millisecond intervals in [RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-17.1.2.2).
TCP and TLS requests expire without retransmission.
Delayed polling schedules from the observation time and avoids a catch-up burst.

`RegistrationIdentity::advance` preserves the boot identifier and increments the checked request sequence.
Request sequences remain below 2³¹.
Serialization preserves the checked registrar and address URI octets.
The request has an empty body and advertises its concrete local signaling address.

Cancellation makes further polling silent and retains `identity` and `expires_at`.
A manager must keep the original settlement boundary for a sent request before starting a replacement transaction at the same registrar.
The [generation decision](adr/own-outbound-registrations-by-configuration-generation.md) defines the pending manager and adapter ownership.

Responses match the peer, top Via branch, `sent-by`, transport, Call-ID, and request sequence and method.
Provisional responses preserve the original timeout and select four-second UDP retry intervals.
`RegistrationResponse` records a matched final response and owns the original request.
Timer K absorbs duplicate final responses for five seconds on UDP.
Reliable transactions finish when the final response arrives.

Authentication, Contact grant interpretation, refresh, retry policy, and network scheduling remain pending.
