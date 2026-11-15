---
date: 2026-11-16
status: proposed
---

# Adopt only observed Stasis channels

## Context

Applications receive incoming channels through [Stasis events](https://docs.asterisk.org/Configuration/Interfaces/Asterisk-REST-Interface-ARI/Introduction-to-ARI-and-Channels/ARI-and-Channels-Manipulating-Channel-State/).
The client owns only channels created through its session.
An app can miss answer or termination events while preparing an incoming call.

## Decision

Keep at most 64 unowned Stasis observations in the existing event driver.
Accept explicit adoption only from the driver's live Stasis observations.
Transfer its observed progress into one existing call lifetime.
Preserve the session's identifier reuse guard and bounded cleanup workers.
Reject unknown, duplicate, departed, and cancelled adoption requests.
Never synthesize HTTP acceptance for an incoming channel.
Keep answer and media observations independent.
Leaving Stasis releases cleanup responsibility.
Observation overflow closes the driver with an identifiable capacity error.

## Alternatives

Raw channel identifiers require no observation storage but lose event ordering and app membership.
Automatically owning every event would end channels the app never adopted.

## Consequences

An app must adopt incoming calls explicitly.
Reconsider the observation bound if measured concurrency requires more channels.

## Confirmation

Test real socket adoption, early answers, departures, duplicate claims, capacity, dropped requests, cancellation, and session shutdown.
