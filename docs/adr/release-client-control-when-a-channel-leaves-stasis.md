---
date: 2026-11-15
status: proposed
---

# Release client control when a channel leaves Stasis

## Context

The [reference event contract](https://github.com/asterisk/asterisk/blob/22.9.0/rest-api/api-docs/events.json) distinguishes leaving Stasis from destroying a channel.
Deleting a channel after `StasisEnd` can end a call that moved to another app or resumed its dialplan.

## Decision

Complete the client handle with `StasisExited` when its channel leaves Stasis.
Release remote cleanup responsibility without sending a hangup request.
Keep `Ended` for a channel destruction event.
Preserve an earlier explicit cancellation and its pending cleanup.
Late events and originate responses can't restore released ownership.

## Alternatives

Treating every Stasis exit as call termination loses the event's meaning.
Keeping cleanup responsibility after exit risks ending the destination app's call.
Ignoring the exit leaves an unobservable handle waiting indefinitely.

## Consequences

Clients matching `CallCompletion` must handle `StasisExited` explicitly.
The result confirms release of client control and makes no claim about later call termination.
Reconsider only with a reference event that identifies a stronger terminal condition.

## Confirmation

Test app moves, dialplan continuation, late originate responses, duplicate exits, cancellation, and handle disposal.
Keep the record proposed until integration and reference checks pass.
