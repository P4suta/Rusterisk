# Music-on-hold settings

`Config::load` reads optional `musiconhold.conf` beside the existing configuration files.
A missing or empty file defines no music classes.
The five-argument `Config::parse` keeps an empty music catalog until `with_music_on_hold_configuration` supplies definitions.

A file class requires `mode=files` and `directory`.
The supported sorting value is `sort=alpha`.
Class names, option names, and supported mode and sort values use ASCII case-insensitive matching, following the [reference implementation](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_musiconhold.c#L1181-L1242).
The parser preserves directory spelling and class-name spelling.
The parser rejects duplicate class names and option names after case folding.
The parser accepts an empty `[general]` section and rejects general settings and other options.

```ini
[default]
mode=files
directory=moh
sort=alpha
```

The project profile admits 32 classes, 79-byte class names, and 255-byte directories.
Class names use the existing ASCII configuration-name syntax.
Directories contain only nonempty normal relative components separated by `/`.
The parser refuses absolute paths, colons, backslashes, `.` components, and `..` components.
Each component follows a cross-platform filename profile derived from [Microsoft's Naming Conventions](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file#naming-conventions).
The profile rejects reserved characters, device basenames including extensions and the superscript `COM` and `LPT` forms, and trailing ASCII spaces or periods.
Leading periods, internal spaces, and Unicode names without excluded characters remain valid.
The limits and relative paths are project policy.
Omitted sorting selects alphabetical order as project policy.
The [Asterisk sample](https://github.com/asterisk/asterisk/blob/22.9.0/configs/samples/musiconhold.conf.sample#L80-L84) leaves omitted order undefined.

Unsupported modes, sorting values, options, duplicates, and invalid bounds report `musiconhold.conf` and the offending line.
Parsing validates definitions without opening audio files or resolving a data directory.
Audio-file validation, bounded inventories, runtime playback, and publication of replacement settings require separate adapters.
The [configuration decision](adr/parse-file-music-classes-before-reading-audio.md) records the schema boundary.
