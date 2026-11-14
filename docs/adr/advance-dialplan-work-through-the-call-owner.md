---
date: 2026-11-15
status: proposed
---

# Advance dialplan work through the call owner

## Context

Dialplan waits, signaling, and Stasis control can affect the same channel.
Waiting inside event reception would prevent progress.

## Decision

Keep the dialplan cursor, suspended operation tokens, branch selection, and terminal cancellation in the pure core.
Let the serialized call controller own each channel before, during, and after Stasis.
Execute network and audio effects in adapters and match completions to the channel generation and operation token.
Preserve the call's starting dialplan through owned bounded snapshots.
Limit immediate instructions per controller turn so loops yield to signaling.

Follow [Goto's context, extension, and priority rules](https://docs.asterisk.org/Asterisk_22_Documentation/API_Documentation/Dialplan_Applications/Goto/).
Use [parallel Dial](https://docs.asterisk.org/Asterisk_22_Documentation/API_Documentation/Dialplan_Applications/Dial/) to select the first answering branch.
Release the remaining branches.

## Alternatives

A task per channel still requires serialized ownership and cancellation messages.
Executing waits in the controller would block independent calls.

## Consequences

The controller needs explicit suspended states and completion tokens.
Reconsider snapshot storage if measured memory exceeds the configured call budget.

## Confirmation

State tests must reject stale completions and prove terminal cancellation.
Real call tests must cover waits, parallel answers, late answers, Stasis transitions, and received audio before acceptance.
