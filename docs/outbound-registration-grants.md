# Outbound registration grants

`RegistrationResponse::grant` interprets a matched successful final response.
It compares the submitted Contact with at most thirty-two response Contacts using the [RFC 3261 comparison rules](https://www.rfc-editor.org/rfc/rfc3261.html#section-19.1.4).
Contacts with other URI schemes don't match the submitted `sip` or `sips` address.
Missing and ambiguous matches reject a positive registration grant.

The matching Contact's `expires` parameter takes priority over the response's `Expires` field, as required by [RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-10.2.4).
The requested interval never substitutes for an absent response interval.
The granted lifetime in seconds must fit an unsigned 32-bit value, and absolute deadlines use checked monotonic milliseconds.
`RegistrationGrant::Active` records `expires_at` and `refresh_at`.
Refresh uses the ten-second buffer from [Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip_outbound_registration.c#L1442), with immediate refresh for shorter grants.

A removal request succeeds only when its Contact is absent or has a zero lifetime.
`RegistrationGrant::Removed` distinguishes removal from an active binding.
Native registration, refresh transmission, retry scheduling, and configuration replacement remain pending.
