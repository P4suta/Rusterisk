---
date: 2026-11-21
status: proposed
---

# Confirm channel answers before replying to clients

## Context

The telephone app bridges an incoming channel after the answer request succeeds.
Bridging a ringing channel fails.
Asterisk 22.9.0 waits for the channel answer command before returning HTTP 204.
See the [answer operation](https://github.com/asterisk/asterisk/blob/22.9.0/res/ari/resource_channels.c) and the [Stasis command](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_stasis_answer.c).

## Decision

Complete the HTTP answer request when the control task observes the same channel generation in the answered state.
Share one signaling command among callers waiting for the same answer.
Keep the command pending after a client disconnects until the last request deadline, channel answer, or hangup.
Keep the existing five-second request deadline and bound the number of pending replies.

## Alternatives

Polling the channel in every client would add requests and require clients to detect a reused channel identifier.
Allowing a ringing channel into the bridge would change media ownership.

## Consequences

A slow signaling adapter delays the HTTP response while the control task continues to process calls.
Media readiness remains a separate event.
Revisit the deadline if measured signaling delays exceed five seconds on supported transports.

## Confirmation

Test bridge admission, retries after disconnect, per-request timeouts, and hangup during an answer.
Verify an authenticated telephone call through hangup.
