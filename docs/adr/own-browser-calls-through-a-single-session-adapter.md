---
date: 2026-11-16
status: proposed
---

# Own browser calls through a single session adapter

## Context

The approved example requires browser calls through `SIP.js`.
Its [session API](https://sipjs.com/guides/end-call/) exposes cancellation and termination for each owned call.
The [capture specification](https://www.w3.org/TR/mediacapture-streams/#dom-mediadevices-getusermedia) exposes permission failures and track lifetimes.

## Decision

Use a typed presentation controller over one `SIP.js` 0.21.2 adapter with individually owned sessions.
Get microphone access only from an explicit call or answer action.
Stop owned tracks after cancellation, failure, disconnect, or hangup, including late capture results.
Keep server call decisions in the Rust core.
Bundle pinned dependencies locally without runtime downloads.
Use labeled native controls and persistent status feedback under [Web Content Accessibility Guidelines 2.2](https://www.w3.org/TR/WCAG22/#status-messages).

## Alternatives

The Simple User API provides smaller integration but its callbacks omit session identity, making late call callbacks harder to isolate.
Direct browser signaling would duplicate protocol behavior.

## Consequences

The example permits one call at a time.
Reconsider the adapter when the example needs transfer controls.
Browser audio and layout checks remain separate from unit contracts.

## Confirmation

Test microphone denial, cancellation, stale completion, and cleanup.
Compare received tones and recording content through actual browser calls.
