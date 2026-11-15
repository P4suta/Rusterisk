---
date: 2026-11-16
status: proposed
---

# Namespace signaling objects by name and type

## Context

The [Asterisk configuration guide](https://docs.asterisk.org/Configuration/Channel-Drivers/SIP/Configuring-res_pjsip/PJSIP-Configuration-Sections-and-Relationships/) gives an endpoint and its address of record the same object name.
The current parser rejects repeated section names across object types.
Correct registration authorization would break the bundled phone configuration.

## Decision

Parse signaling settings with a dedicated object namespace.
Identify each object by its section name and declared type.
Reject duplicate names within the same type and missing or unsupported types.
Preserve source positions, option uniqueness, and existing input limits.
Keep the general settings parser's section names unique.
Keep duplicate management users and transport objects invalid.
Resolve references through their declared object types.

## Alternatives

Renaming every endpoint changes configured dialing targets.
Allowing duplicate sections in every configuration file weakens unrelated credential validation.
Merging repeated objects hides conflicting settings.

## Consequences

Phone addresses can match their registration targets while endpoint names remain stable.
Distinct types never overwrite one another.

## Confirmation

Test shared names across endpoint, credentials, and address objects.
Reject duplicate objects, management users, missing types, and repeated options with source positions.
