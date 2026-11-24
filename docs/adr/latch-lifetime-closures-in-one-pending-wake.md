---
date: 2026-11-24
status: proposed
---

# Latch lifetime closures in one pending wake

## Context

The control task polled every 100 ms for dropped subscriptions, released media leases, and abandoned requests.
A dropping guard can't fail or wait, and a closure needs no payload because the task rescans its state.

## Decision

Give the control task a one-slot latch.
Each guard marks it after its closed state becomes visible, and the task reads it before rescanning.
A full latch already holds a wake that covers the new closure.
The client session latches closed call handles likewise.

## Alternatives

A reserved queue slot per guard refuses registrations when slots run out and shares capacity with abandoned requests.
Polling costs ten idle turns per second.
A shared notifier needs shared ownership that project rules reject.

## Consequences

Each closure costs one full rescan.
Declaration order places the closed state before the latch, which a single-threaded test can't observe.
Reconsider when a notice must name its call slot.

## Confirmation

Tests must show that guards beyond any queue size leave one pending wake.
Tests must show that a dropped subscription and an abandoned request end without a timer.
