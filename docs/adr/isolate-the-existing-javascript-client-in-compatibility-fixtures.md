---
date: 2026-11-15
status: proposed
---

# Isolate the existing JavaScript client in compatibility fixtures

## Context

The approved plan requires existing `node-ari-client` compatibility.
The published `ari-client` 2.2.0 uses shared Swagger registries and legacy dependencies.
The dependency audit reports issues including [multipart boundary generation](https://github.com/advisories/GHSA-fjxv-7rqg-78g4) and [redirect handling](https://github.com/advisories/GHSA-p8p7-x288-28g6).

## Decision

Keep the historical client inside a private comparison fixture.
Run each comparison in an owned process with a deadline and synthetic credentials.
Accept only loopback management targets.
Use fixed operations and validate response snapshots before comparing contract fields.
Check owned TypeScript with strict compiler and semantic lint rules.
Keep the client outside server and Rust client dependencies.
Review dependency findings for the exact fixture operations before completing dependency assurance.

## Alternatives

Replacing the client tests a different consumer.
Overriding its HTTP implementation changes the historical dependency boundary.
Using unchecked scripts leaves malformed responses and missing types undetected.

## Consequences

Legacy dependency findings need a scoped assessment.
Reassess the boundary before accepting remote targets, redirects, cookies, or multipart inputs.

## Confirmation

Test target rejection, response validation, annotation enforcement, both servers, process deadlines, and cleanup.
Keep the record proposed while dependency assessment remains incomplete.
