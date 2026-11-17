---
date: 2026-11-18
status: proposed
---

# Use project service without glob project resolution

## Context

The comparison fixture and browser example use `projectService: true`.
[The pinned parser bypasses glob project resolution](https://github.com/typescript-eslint/typescript-eslint/blob/v8.72.2/packages/typescript-estree/src/parseSettings/createParseSettings.ts#L210-L218).
Its glob resolver brings [unpatched picomatch repetition](https://github.com/advisories/GHSA-fc5q-39xc-vjpf).

## Decision

Replace that dependency with one private adapter shared by both fixtures.
Export `isDynamicPattern` and `globSync`, throwing an identifiable unsupported error whenever invoked.
Preserve typed linting and its rejecting fixtures.
Require `projectService: true` and reject `parserOptions.project` in both fixture profiles.
Validate the exact local path, manifest, runtime resolution, bounded source inventory, and captured sources through maintained gates.
Keep existing advisory allowances unchanged.

## Alternatives

Waiting for an upstream release leaves security checks failing.
[Partial repetition rewrites](https://github.com/micromatch/picomatch/pull/246) leave other patterns unresolved and change capture behavior.
A general glob replacement introduces unnecessary matching semantics.

## Consequences

Explicit project lists become unsupported.
[npm permits scoped local overrides](https://docs.npmjs.com/cli/v11/configuring-npm/package-json#overrides), but [audit excludes linked packages](https://github.com/npm/cli/blob/v11.20.0/workspaces/arborist/lib/audit-report.js#L353-L362).
Independent source validation remains necessary.
Reconsider after an upstream release passes equivalent checks.

## Confirmation

Require clean installation, unchanged semantic lint rejection, adapter-call rejection, tampering and redirected-link rejection, and complete source capture.
Restrict comparison audit findings to the existing nine legacy packages and require no browser audit findings.
