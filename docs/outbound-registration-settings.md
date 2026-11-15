# Outbound registration settings

Registration settings use `type=registration` sections in `pjsip.conf`.
The running registration client remains pending.

Section names are unique within the registration object type.
`transport` selects a configured signaling transport.
`outbound_auth` names one authentication record.
`server_uri` identifies the registrar, and `client_uri` identifies the address of record.
Each section requires all four settings.
The parser admits at most 16 registration sections.

`expiration` defaults to 3600 seconds, and `contact_user` defaults to `s`.
Contact users follow Session Initiation Protocol (SIP) URI user syntax within 256 ASCII bytes.
Percent escapes require two hexadecimal digits.

`retry_interval` defaults to 60 seconds.
`forbidden_retry_interval` and `fatal_retry_interval` default to zero.
`max_retries` defaults to 10, and zero requests no retries.
`max_random_initial_delay` defaults to 10 seconds.
`auth_rejection_permanent` defaults to `yes`.
The defaults follow [Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip_outbound_registration.c#L2790-L2804).

Timer values use unsigned 32-bit seconds.
Invalid values and capacity overflow report their file and line.
Complete registration URI validation and escaped semicolon handling remain pending.
