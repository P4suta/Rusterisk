---
date: 2026-11-15
status: proposed
---

# Preserve existing discovery clients

## Context

[Asterisk 22.9.0 discovery](https://raw.githubusercontent.com/asterisk/asterisk/22.9.0/rest-api/api-docs/channels.json) declares Swagger 1.1.
[The existing client](https://raw.githubusercontent.com/asterisk/node-ari-client/master/lib/resources.js) recognizes `List[...]` when wrapping resource lists.
[Swagger 1.2](https://raw.githubusercontent.com/swagger-api/swagger-spec/master/versions/1.2.md) describes arrays through `type` and `items`.
The approved plan requires existing clients and standard definitions.

## Decision

Serve Asterisk discovery at `/ari/api-docs` and standard Swagger at `/rusterisk/api-docs`.
Generate both from the same operations and Rust models.
Keep OpenAPI 3.1 at `/rusterisk/openapi.json`.

## Alternatives

Standard arrays at the Asterisk path would lose the client's resource wrapping.
Modifying the client would require another client distribution.

## Consequences

Two renderers must preserve the same operations and event models.
Each response identifies its actual format.
Reconsider separate discovery paths when the existing client supports standard arrays.

## Confirmation

Check both declarations and exercise event resources and nonempty lists through the existing client.
Keep this record proposed until reference comparisons and client execution pass.
