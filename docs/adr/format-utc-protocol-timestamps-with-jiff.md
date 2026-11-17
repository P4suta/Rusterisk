---
date: 2026-11-18
status: proposed
---

# Format protocol timestamps with Jiff

## Context

[RustSec](https://github.com/RustSec/advisory-db/blob/main/crates/chrono/RUSTSEC-2026-0543.md) marks every Chrono version unmaintained.
The [maintainer recommends Jiff](https://github.com/chronotope/chrono/issues/1768).
Existing server responses and events require stable timestamp strings.

## Decision

Use server-only Jiff 0.2.45 with `default-features=false` and `features=["std"]`.
Preserve Coordinated Universal Time (UTC) and RFC 3339 output.
Startup and ping keep `+00:00` and Chrono's `AutoSi` precision: zero, three, six, or nine fractional digits.
Events keep exactly three fractional digits, truncate finer precision, and end in `Z`.
Select explicit precision through Jiff's formatting API.
Keep the dependency-free Core unchanged.

## Alternatives

Already-locked time 0.3.55 adds no package but requires custom format descriptions and fallible formatting integration.
Keeping Chrono leaves the security gate failing.

## Consequences

Accept Jiff's dependency additions while excluding system time zone discovery and bundled zone data.
General parser equivalence remains outside the contract.
Reconsider for non-UTC protocol requirements.

## Confirmation

Compare fixed-instant golden strings, including trailing zeros and the instant before the Unix epoch.
Check generated startup parsing, info stability, ping, events, supported targets, and security.
Keep proposed pending execution.
