# Incoming dialog targets

Incoming Session Initiation Protocol (SIP) dialogs capture an owned target from the bounded Contact parser.
The target profile rejects `transport=tls`, incompatible transports, and unimplemented `maddr` or `ttl` routing.
An incoming `INVITE` with an unusable target receives `400` before dialog admission.
Receive grammar remains separate from the generated target profile.

The owned target ignores method parameters and URI headers disallowed in dialog Contacts by [RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-19.1.1).
Ignored components don't replace generated methods, message bodies, or headers.
Allowed URI parameters remain part of the target.

Target refresh preserves the initiating dialog's URI security under [RFC 5630](https://www.rfc-editor.org/rfc/rfc5630.html#section-5.1.2).
Rejected replacements preserve the previous target, media direction, and remote sequence number.
Notification target replacements receive the same checks before advancing the local sequence number.
Outgoing response targets and failure handling for mandatory `ACK` requests remain pending in the [proposed decision](adr/validate-dialog-targets-before-state-changes.md).
