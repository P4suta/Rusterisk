---
date: 2026-11-15
status: proposed
---

# Connect configured WebSocket clients with owned jobs

## Context

[Asterisk 22.9](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_websocket_client.c) creates per-call connections with bounded attempts and configured delays.
Connection setup must preserve signaling progress and a call's starting configuration.

## Decision

Copy the selected client configuration into the call's connection effect.
Prepare immutable TLS trust during startup, before admission timers run.
Run each connection in an owned, cancellable job with a four-second admission deadline.
Verify TLS certificates, hostnames, and the negotiated `media` subprotocol.
Bind attachment, answer, and received audio to the channel generation.
Keep credentials and destination URIs out of transport errors and normal logs.

## Alternatives

Connecting in the signaling loop would delay unrelated calls.
Reading mutable configuration during retries would change an active call's destination or credentials.
Unbounded retries would keep cancelled calls alive.

## Consequences

Configuration validation bounds the complete retry schedule.
Admission, answer, and received audio remain distinct observations.
Cancellation and failed admission release the owned socket and task.

## Confirmation

Tests check configured requests, protocol refusal, retries, cancellation, generation reuse, and received audio.
Reference comparisons exercise outgoing connections through both servers.
The record stays proposed pending integration and review.
Reconsider the deadline when reference coverage needs a longer validated retry schedule.
