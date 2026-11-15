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

Use a typed presentation controller over one `SIP.js` 0.21.2 adapter with owned sessions.
Get microphone access only from an explicit call or answer action.
Stop owned tracks after cancellation, failure, disconnect, or hangup, including late capture results.
Keep server call decisions in the Rust core.
Bundle pinned dependencies locally with the [publisher's license notice](https://github.com/onsip/SIP.js/blob/0.21.2/LICENSE.md).
Use native controls with visible [labels](https://www.w3.org/WAI/tutorials/forms/labels/#associating-labels-explicitly), [groups](https://www.w3.org/WAI/tutorials/forms/grouping/#associating-related-controls-with-fieldset), persistent [errors](https://www.w3.org/WAI/tutorials/forms/notifications/#in-line-feedback), and [status](https://www.w3.org/TR/WCAG22/#status-messages).
Preserve [authentication help](https://www.w3.org/TR/WCAG22/#accessible-authentication-minimum), native [playback control](https://www.w3.org/TR/WCAG22/#audio-control), [reflow](https://www.w3.org/TR/WCAG22/#reflow), [enlargement](https://www.w3.org/TR/WCAG22/#resize-text), and [spacing](https://www.w3.org/TR/WCAG22/#text-spacing).
Apply project defaults of 18 pixels, 1.75 line height, and 48-pixel targets.

## Alternatives

The Simple User API provides smaller integration but its callbacks omit session identity, making late call callbacks harder to isolate.
Direct browser signaling would duplicate protocol behavior.

## Consequences

The example permits one call at a time.
Reconsider the adapter for transfer controls.
Separate browser audio and layout checks from unit contracts.

## Confirmation

Test microphone denial, cancellation, stale completion, and cleanup.
Compare received tones and recording content through actual browser calls.
