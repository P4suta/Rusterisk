---
date: 2026-11-16
status: proposed
---

# Retry complete native trust loads before admission

## Context

macOS intermittently returns an I/O error from [SecTrustSettingsCopyCertificates](https://developer.apple.com/documentation/security/sectrustsettingscopycertificates(_:_:)).
The [native loader](https://docs.rs/rustls-native-certs/0.8.5/src/rustls_native_certs/macos.rs.html) can return certificates from other domains alongside that failure.
Using that partial result can lose user distrust settings.

## Decision

Prepare native trust during startup.
Attempt each complete read at most three times, waiting 100 milliseconds between failures.
Accept only one complete result and discard failed results.
Reject startup after the final failure.
Keep error categories and OS status without paths or certificate contents.
Preserve certificate and peer-name verification.

## Alternatives

Immediate failure makes transient read errors fatal.
Using partial results can broaden platform trust.
Retrying during admission blocks call progress.

## Consequences

Startup can make three platform calls and wait 200 milliseconds.
The platform API provides no cancellation boundary.
Reconsider the retry limit if measured startup failures persist.

## Confirmation

Injected reads must recover from incomplete results without merging certificates.
Persistent failure must exhaust exactly three reads and two waits.
Real handshakes must still reject untrusted certificates and mismatched names.
