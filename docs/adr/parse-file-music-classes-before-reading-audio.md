---
date: 2026-11-17
status: proposed
---

# Parse file music classes before reading audio

## Context

[Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/configs/samples/musiconhold.conf.sample) configures file classes in `musiconhold.conf` and leaves omitted sorting undefined.
Configuration parsing has no data-directory argument.

## Decision

Keep the five-argument parser and load the optional file into checked, privately constructed classes.
Support `mode=files`, a preserved relative directory, and `sort=alpha`.
Use alphabetical sorting when omitted as project policy.
Match class names and supported keywords without ASCII case sensitivity.
Reject unsupported settings at their source lines.
Bound the profile to 32 classes, 79-byte names, and 255-byte directories with [portable relative components](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file#naming-conventions).
The bounds and absolute-path refusal are project policy.
Parse definitions without reading audio.
Require separate storage preparation before runtime publication.

## Alternatives

Reading audio during parsing would couple a reusable schema to a missing data root.
Ignoring unsupported settings would accept unavailable behavior.
Unrestricted reference settings need other adapters.

## Consequences

Missing configuration creates no implicit class.
Asset validation and live replacement remain separate work.
Reconsider the profile when applications need another mode or external directory.

## Confirmation

Check public loading, positioned refusals, capacity boundaries, case preservation, and parsing without audio files.
Keep the record proposed pending the runtime adapter.
