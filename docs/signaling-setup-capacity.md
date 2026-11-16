# Signaling setup capacity

Network owns sixteen setup keys shared by calls and registration lookups.
Setup for an existing registered flow reserves a key without reserving another connection.
Network accepts resolution metadata when sixty-four streams are open.
Connecting preparations and established streams share sixty-four connection slots.
Network reuses matching flows before another connection attempt.
Reuse preserves the resolved candidate order.

Cancellation preserves setup keys until the event loop processes each completion.
Connection admission replaces a preparation with an established flow and releases its setup key.
Reuse releases setup metadata after its resolver completion without closing the shared flow.
Generation checks prevent delayed completions from releasing another setup's key.
The bounds follow the [configuration generation decision](adr/own-outbound-registrations-by-configuration-generation.md).
