---
date: 2026-11-21
status: proposed
---

# Verify test call audio at the nominated channel

## Context

Accepting an outgoing call, receiving an answer, and receiving echoed audio occur at different times.
The [Asterisk Echo application](https://docs.asterisk.org/Latest_API/API_Documentation/Dialplan_Applications/Echo/) returns voice frames to the caller.

## Decision

Give each probe a private Stasis application and one explicitly nominated outgoing channel.
Bind its synthetic challenge and receiver to the channel generation.
Inject the challenge into the outgoing media path and compare incoming audio at the media adapter.
Close the application when the request ends or its deadline expires.

## Alternatives

A shared bridge would add a second call leg and a second cleanup operation.
Counting incoming packets would also accept silence or unrelated sound.

## Consequences

The CLI can use authenticated management requests without linking server media libraries.
A verified result identifies media returned during one test call.

## Confirmation

Exercise an echoing peer, a busy peer, no answer, silence, and mismatched audio.
Keep an unrelated call active during cancellation and a late answer.
