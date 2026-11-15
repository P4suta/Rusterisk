---
date: 2026-11-15
status: proposed
---

# Route resource subscriptions within control turns

## Context

Explicit event subscriptions observe resources without granting Stasis control.
[Asterisk subscription handlers](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_stasis.c) process source lists in order.
[Resource forwarding](https://github.com/asterisk/asterisk/blob/22.9.0/res/stasis/app.c) counts interests and removes them when resources end.

## Decision

Keep bounded interests under the serialized control task.
Use resource generations and app generations to identify interests.
Route resource notifications before removing ended interests within the same control turn.
Keep directed Stasis lifecycle events with their controlling app.
Count repeated subscriptions without granting channel control.
Apply filters before delivery and send each notification once per recipient.

## Alternatives

Separate forwarding tasks would require coordinating deletion with identifier reuse.
Treating subscriptions as Stasis membership would give observers control and change disconnect behavior.

## Consequences

Resource termination clears specific interests before identifier reuse.
Wildcard interests survive resource termination.
Bounded recipient queues preserve existing disconnect behavior.
Reconsider the design if measured routing cost exceeds control latency limits.

## Confirmation

Check duplicate interests, partial batches, observers, filters, deletion, identifier reuse, and disconnects with the reference.
Keep the record proposed until call comparisons pass.
