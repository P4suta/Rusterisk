---
date: 2026-11-15
status: proposed
---

# Bind endpoint observations to configuration generations

## Context

[Endpoint availability](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip/pjsip_configuration.c) describes routing availability independently of an active call.
The approved reload contract preserves each call's starting settings.
Delayed contact observations must not update replacement configuration.

## Decision

Project endpoint resources from configured names, contact presence, and explicit channel membership.
Tag contact observations with an endpoint name and configuration generation.
Apply observations only to the matching configuration.
Combine static routes and registered contacts when computing availability.
Keep existing calls when a contact expires.

## Alternatives

Updating by name alone can apply stale observations after reload.
Deriving membership from display names couples routing to presentation.
Ending calls on registration expiry confuses registration and dialog lifetimes.

## Consequences

Configuration replacement must advance the observation generation.
Public resource names remain stable across configuration changes.
Reconsider the projection when supporting another endpoint technology.

## Confirmation

Test static routes, contact changes, stale observations, exact membership, missing resources, and calls that survive contact expiry.
Keep the record proposed until reference and reload checks pass.
