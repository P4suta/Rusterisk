# Registration contact security

The registrar validates URI security roles before preparing a binding update.
A `sips` Contact requires a `sips` Request-URI and the `sips` scheme in every Contact and Path URI under [RFC 5630, Section 5.2](https://www.rfc-editor.org/rfc/rfc5630.html#section-5.2).
The registrar's role check excludes From and To URI schemes, as specified in that section.
A `sips` Request-URI with a `sip` Contact receives `400`, following [Section 5.1.2](https://www.rfc-editor.org/rfc/rfc5630.html#section-5.1.2).

Role conflicts receive `400` without changing bindings or publishing presence changes.
Path validation applies to updates containing a `sips` Contact and accepts at most 32 URIs through the bounded entry splitter and the locked Record-Route parser.
Values require `name-addr` syntax and the `lr` routing indicator under [RFC 3327](https://www.rfc-editor.org/rfc/rfc3327.html#section-4).
Invalid Path values in a `sips` Contact update receive `400` without changing bindings or publishing presence changes.
Queries and wildcard removal preserve their existing behavior.
Path routing remains pending.
