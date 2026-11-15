---
date: 2026-11-15
status: proposed
---

# Tie recording prompts to the recording lifetime

## Context

[Asterisk recording](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_stasis_recording.c) publishes `RecordingStarted` before playing the recording beep.
Waiting in the control actor would delay cancellation.

## Decision

Reserve a native playback slot under the recording token before opening the beep file.
The capture owns its playback completion receiver.
Poll completion in the media loop while continuing controls and signaling.
Start capture timing and input storage after the prompt completes.
Releasing the capture drops the receiver and stops its prompt.
Use the configured `sound:beep` resource without publishing a separate playback resource.

## Alternatives

Waiting in a signaling task needs another cancellation path while attachment is pending.
Generating a fixed tone would ignore the configured sound.

## Consequences

A prompted recording also needs one bounded playback slot.
Reconsider the shared capacity if prompts prevent normal playback admission.

## Confirmation

Test successful and failed prompts, cancellation, timing, and resource release.
Compare actual received prompt audio and saved input audio.
Keep the record proposed until reference and platform checks pass.
