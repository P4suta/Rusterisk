---
date: 2026-11-16
status: proposed
---

# Check pinned signaling declarations with explicit undefined

## Context

`SIP.js` 0.21.2 declares optional delegates in [incoming](https://github.com/onsip/SIP.js/blob/0.21.2/src/core/messages/incoming-request.ts), [outgoing](https://github.com/onsip/SIP.js/blob/0.21.2/src/core/messages/outgoing-request.ts), [invitation](https://github.com/onsip/SIP.js/blob/0.21.2/src/core/messages/methods/invite.ts), and [subscription requests](https://github.com/onsip/SIP.js/blob/0.21.2/src/core/messages/methods/subscribe.ts).
Its concrete classes also expose `undefined`.
TypeScript's [exact optional property checking](https://www.typescriptlang.org/tsconfig/exactOptionalPropertyTypes.html) distinguishes those contracts.
The unmodified package fails the example's strict check.

## Decision

Keep strict checking for all project and dependency declarations.
Correct only the four delegate declarations in the example's installed 0.21.2 package to explicitly include `undefined`.
Verify the package identity, fixed paths, original SHA-256 digests, exact occurrence count, and corrected declarations before writing.
Accept an identical correction on repeated runs.
Reject every other source change.

## Alternatives

Disabling declaration checking would hide unrelated dependency errors.
Weakening optional property checking would reduce the project's contract.
A private fork would replace the published dependency for four declaration lines.

## Consequences

The maintained browser gate corrects its own installation after dependency setup.
Reconsider the correction when the pinned upstream package changes.

## Confirmation

Reject other versions, fields, extra occurrences, and altered declarations.
Run the strict compiler over the corrected actual package.
