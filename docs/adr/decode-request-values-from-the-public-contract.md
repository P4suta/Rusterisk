---
date: 2026-11-16
status: proposed
---

# Decode request values from the public contract

## Context

[Asterisk request callbacks](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_ari_bridges.c) apply JSON query fields after URL query fields.
Path identifiers keep their routing role.
Channel creation also accepts a distinct variables object.

## Decision

Use the operation catalog's named parameter kinds for request decoding and generated schemas.
Decode registered JSON fields after authentication and Origin validation.
Preserve path identifiers and ignore fields outside the operation contract.
Keep channel variables distinct from scalar control arguments.
Bound bodies, field counts, lists, keys, and values before control admission.
Preserve identifiable unsupported responses for missing adapters.

## Alternatives

Independent handler decoders would duplicate types and precedence rules.
Treating JSON values as strings would lose numeric, Boolean, and list semantics.
Applying variables through later HTTP requests would expose channels before their initial configuration finishes.

## Consequences

The same public contract describes both request forms.
Creation must admit variables atomically with its channel generation.
Reconsider conversions when the pinned reference changes.

## Confirmation

Compare body precedence, path isolation, typed values, variable admission, rejection errors, and event snapshots with the reference.
Keep the record proposed while integration findings remain.
