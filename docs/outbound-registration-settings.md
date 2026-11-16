# Outbound registration settings

Registration settings use `type=registration` sections in `pjsip.conf`.
The [startup adapter](outbound-registration-startup.md) connects to numeric addresses over User Datagram Protocol (UDP).

Section names are unique within the registration object type.
`transport` selects a configured signaling transport.
`outbound_auth` names one authentication record.
`server_uri` identifies the registrar, and `client_uri` identifies the address of record.
Each section requires all four settings.
The parser admits at most 16 registration sections.

`RegistrarUri` and `RegistrationAddress` preserve the original URI octets within 2048 ASCII bytes.
The settings profile admits UDP, TCP, and TLS registrar targets.
The `sips` scheme requires TLS.
TLS uses `transport=tcp` when the URI parameter is present.
The URI profile rejects `transport=tls` before request generation.
With a `sips` registrar, the single Contact requires a `sips` address of record.
A `sips` Contact also requires a `sips` registrar Request-URI under [RFC 5630](https://www.rfc-editor.org/rfc/rfc5630.html#section-5.2).
Registrar URIs exclude user information.
Addresses of record exclude explicit ports and transport parameters, following the component roles in [RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-19.1.2).
The profile rejects passwords, URI headers, routing and method parameters, malformed escapes, and duplicate parameter names.
It retains parser-supported extension parameters.

`expiration` defaults to 3600 seconds, and `contact_user` defaults to `s`.
Contact users follow Session Initiation Protocol (SIP) URI user syntax within 256 ASCII bytes.
Percent escapes require two hexadecimal digits.
Use `\;` for literal semicolons in configuration values.
The parser removes only the immediately preceding backslash, following the [reference lexer](https://github.com/asterisk/asterisk/blob/22.9.0/main/config.c#L2399-L2405).

`retry_interval` defaults to 60 seconds.
`forbidden_retry_interval` and `fatal_retry_interval` default to zero.
`max_retries` defaults to 10, and zero requests no retries.
`max_random_initial_delay` defaults to 10 seconds.
`auth_rejection_permanent` defaults to `yes`.
The defaults follow [Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip_outbound_registration.c#L2790-L2804).

Timer values use unsigned 32-bit seconds.
Invalid values and capacity overflow report their file and line.
Reference lookup accepts later declarations and reports unresolved names at their option lines.
Retry policy, hostname and stream transport setup, and replacement publication remain pending.
