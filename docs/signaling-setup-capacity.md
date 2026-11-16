# Signaling setup capacity

Network owns sixteen setup keys shared by calls and registration lookups.
Setup for an existing registered flow reserves a key without reserving another connection.
New connection preparations also count toward the sixty-four connection limit.

Cancellation preserves setup keys until the event loop processes each completion.
Connection admission replaces a preparation with an established flow and releases its setup key.
Generation checks prevent delayed completions from releasing another setup's key.
The bounds follow the [configuration generation decision](adr/own-outbound-registrations-by-configuration-generation.md).
