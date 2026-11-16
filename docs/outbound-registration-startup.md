# Outbound registration startup

Signaling startup connects numeric-address User Datagram Protocol (UDP) registration targets to the existing listener.
Each request advertises the listener's reachable address and preserves the configured Request-URI octets.
The starting configuration owns at most sixteen clients.
Configurations without registrations start no registration work.

Initial requests follow the [Asterisk 22.9.0 delay](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip_outbound_registration.c#L2005).
`max_random_initial_delay=0` still waits one second.
Positive values select an integer delay from one through `max_random_initial_delay` seconds.
Round-robin polling emits at most one packet per signaling tick.
Requests to the same physical registrar wait for the preceding transaction's settlement deadline.
The existing bounded transaction supplies retransmission and a 32-second timeout.
Shutdown cancels waiting and transmitted clients before releasing the listener.

Hostname targets and TCP or TLS registration targets return `ControlError::Unsupported` during binding until their adapters connect.
Response authentication, successful grant handling, refresh transmission, retry policy, and configuration replacement remain pending in the driver.
The [pure transaction APIs](outbound-registration-transactions.md) and [generation decision](adr/own-outbound-registrations-by-configuration-generation.md) define the remaining integration boundaries.
