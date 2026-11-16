# Dialog targets

Session Initiation Protocol (SIP) dialogs capture an owned target from the bounded Contact parser.
The target profile rejects `transport=tls`, incompatible transports, and unimplemented `maddr` routing.
An incoming `INVITE` with an unusable target receives `400` before dialog admission.
Receive grammar remains separate from the generated target profile.

The owned target ignores method parameters, `ttl` parameters, and URI headers disallowed in dialog Contacts by [RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-19.1.1).
Ignored components don't replace generated methods, message bodies, or headers.
Allowed URI parameters remain part of the target.

Target refresh preserves the initiating dialog's URI security under [RFC 5630](https://www.rfc-editor.org/rfc/rfc5630.html#section-5.1.2).
Rejected replacements preserve the previous target, media direction, and remote sequence number.
Notification target replacements receive the same checks before advancing the local sequence number.

A successful outgoing response needs one usable Contact before media publication.
An unusable target ends the local call without guessing an acknowledgment destination.
Later responses remain absorbed for 32 seconds, and expiry releases the bounded outgoing slot.
Non-successful response Contacts don't replace the dialog target or prevent Digest retries.

Each outgoing call owns one final response and its `ACK` for 32 seconds.
Matching retransmissions reuse the acknowledgment without replacing a refreshed dialog target.
Expiry releases the retained response and acknowledgment while preserving an active dialog.
Forked dialogs and mandatory `ACK` handling for unusable response targets remain unresolved in the [proposed decision](adr/validate-dialog-targets-before-state-changes.md).
