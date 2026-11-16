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

TLS registration targets return `ControlError::Unsupported` during binding until their adapter connects.
The driver uses matching provisional and final responses, the shared Digest implementation, and validated Contact grants.
Successful grants schedule refresh requests with renewed authorization counters.
Retry policy and configuration replacement remain pending in the driver.
The [pure transaction APIs](outbound-registration-transactions.md) and [generation decision](adr/own-outbound-registrations-by-configuration-generation.md) define the remaining integration boundaries.
