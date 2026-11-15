# Existing client dependency scope

The private comparison fixture pins `ari-client` 2.2.0 and its locked dependencies.
The historical client uses legacy HTTP and Swagger packages with known advisories.
The comparison runs fixed resource queries on the owned Rusterisk listener and isolated Asterisk 22.9.0 container.
The wrapper validates loopback targets and response snapshots, uses synthetic credentials, and bounds the child process to 30 seconds and a 128 MiB JavaScript heap.

The dependency gate requires the reviewed package versions, registry archive locations, integrity fields, and development-only installation.
It rejects production dependencies, unknown vulnerable packages, new advisory identifiers, cycles in dependency causes, and inconsistent audit totals.
The gate reports scoped findings rather than claiming that the historical dependencies have no vulnerabilities.
Changes to fixture operations require a fresh semantic review of the dependency boundary.

## Multipart inputs

The fixture operations use fixed query parameters and JSON responses.
The scenarios don't construct multipart requests.
The [predictable multipart boundary](https://github.com/advisories/GHSA-fjxv-7rqg-78g4) and [multipart field injection](https://github.com/advisories/GHSA-hmw2-7cc7-3qxx) conditions remain outside the exercised operations.

## Redirects and cookies

The wrapper accepts only loopback management origins.
Both fixture servers use HTTP Basic authentication and return resource responses directly.
The scenarios exercise neither redirect responses nor cookie input.
The scoped findings cover [cross-protocol redirects in request](https://github.com/advisories/GHSA-p8p7-x288-28g6), [cookiejar parsing](https://github.com/advisories/GHSA-h452-7996-h45h), and [tough-cookie prototype handling](https://github.com/advisories/GHSA-72xf-g2v4-qvf3).
Remote origins, redirects, or cookie scenarios require another assessment.

## Query parameters and identifiers

The resource operations send fixed scalar parameters.
The scenarios don't parse and reserialize hostile nested query objects or increase query parser limits.
The scoped findings cover [array limits](https://github.com/advisories/GHSA-6rw7-vpxm-498p) and [constructor-based type checks](https://github.com/advisories/GHSA-4mjr-xmp4-gh2g) in `qs`.

The existing client's resource constructor calls `uuid.v4()` without a caller-supplied buffer.
The [`uuid` buffer advisory](https://github.com/advisories/GHSA-w5hq-g745-h8pq) concerns other generators with supplied buffers.
The fixture keeps the historical identifier dependency inside the same boundary.

## Tool dependencies

The development lint tool uses a pinned `minimatch` override that addresses its advisory.
Strict compiler and lint checks, their rejecting fixtures, and the resource comparison pass with the override.
Vulnerabilities in unrelated development tools fail the dependency gate.

## License metadata

The gate checks every locked fixture dependency's license metadata and development role.
The permitted expressions cover the fixture's permissive licenses, including the BSD alternative in `json-schema`.
Private build tools also use the [Blue Oak Model License 1.0.0](https://blueoakcouncil.org/license/1.0.0).

The lock file omits license fields for three historical archives.
The packaged declarations identify [`ax` 0.1.8](https://registry.npmjs.org/ax/-/ax-0.1.8.tgz) and [`shred` 0.8.10](https://registry.npmjs.org/shred/-/shred-0.8.10.tgz) as `MIT`.
The source header in [`sprintf` 0.1.1](https://registry.npmjs.org/sprintf/-/sprintf-0.1.1.tgz) supplies the BSD three-clause terms.
The gate limits each clarification to the reviewed package path and version.
It also recognizes the noncanonical Apache 2.0 metadata in `btoa` 1.1.1 and `swagger-client` 2.0.26.
Missing or unreviewed license metadata fails the gate.
