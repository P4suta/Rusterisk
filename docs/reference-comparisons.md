# Reference comparisons

The comparison target is [Asterisk 22.9.0](https://github.com/asterisk/asterisk/tree/22.9.0).
The reference image builds the official source archive after checking its published `SHA-256` digest.
The base image, BuildKit image, and direct system packages use fixed versions.
The image stays local.

## Build the reference

Preview the commands before building.

```sh
mise x -- just reference-build
mise x -- just reference-build --run
```

The dedicated builder has two processors, 2 GiB of memory without swap, and a 256-process limit.
The build deadline is 30 minutes, and compilation uses two jobs.
After successful creation, the runner removes the builder after a completed build, failure, or timeout.
The Dockerfile also checks that the installed binary reports `Asterisk 22.9.0`.
The successful local rehearsal used Linux ARM64 through OrbStack on macOS.

## Compare resources

Preview and execute the comparison with the built reference image.

```sh
mise x -- just interop --dry-run
mise x -- just interop
```

The reference container uses one processor, 512 MiB of memory without swap, and a 128-process limit.
The runner mounts the root filesystem and configuration without write access.
Temporary runtime directories have bounded memory filesystems.
Docker assigns an unused HTTP port on `127.0.0.1`.
The scenario uses synthetic credentials.
The container stops after ten minutes even if the client runner disappears.
The runner waits up to 90 seconds for the control socket and the `core waitfullybooted` reply under one deadline.
The test command has a five-minute deadline.
The runner collects reference logs after a test failure and removes its container after every result.
Each run copies the reference configuration into an owned, bounded temporary directory.
The outgoing media comparison writes its ephemeral callback address into the copy and reloads the reference module.
The runner removes the configuration copy after every result.

The same Rust client queries both servers.
The fast gate checks the disposition of every method and path in the [Asterisk 22.9.0 operation fixture](../crates/api/tests/fixtures/asterisk-22.9.0-routes.tsv).
The live comparison fetches all 11 discovery documents from the reference and compares their 102 operations with the fixture by method, path, name, and response class.
It checks the names, locations, types, required flags, and multiplicity of all 256 [parameter entries](../crates/api/tests/fixtures/asterisk-22.9.0-parameters.tsv).
The workspace tests require every parameter of a declared operation to appear in discovery or have an explicit unavailable status.
It also compares the 43 event types published by the reference with the [event fixture](../crates/api/tests/fixtures/asterisk-22.9.0-events.txt).
Requests to both servers compare HTTP status, the `Allow` header, and error messages for reserved paths, unsupported methods, and unknown resources.
The comparison sends `OPTIONS` to all 72 distinct resource paths on both servers.
The reference image omits the optional mailbox module, so its two mailbox paths return `404`.
Rusterisk recognizes both published paths and reports their declared methods.
An owned Node.js process also uses `ari-client` 2.2.0 to discover operations, subscribe over WebSocket, and compare endpoint and app resources.
The process has a 30-second deadline and a 128 MiB JavaScript heap limit.
Each client operation has a five-second deadline that names the unfinished stage.
The scenario repeats discovery and resource queries five times in one process.
The fast gate checks the TypeScript fixture with its compiler, ESLint, Biome, and rejecting tests.
The [historical dependency boundary](legacy-client-dependency-scope.md) scopes the legacy client's advisory and license checks.
The scenario compares empty channel lists, configured endpoints, static and dynamic routing availability, technology-name case handling, app subscriptions, and missing-resource errors.
The test normalizes endpoint list order by resource name.
The test compares endpoint detail queries and error messages without normalization.
Both clients compare event filter updates and reset.
The Rust comparison also checks invalid filters and missing apps.
Both clients compare endpoint subscriptions and removal.
The Rust client checks technology subscription names, duplicate bridge interests, partial source batches, and deletion cleanup.
The test explicitly compares successful Rusterisk wildcard removal with the reference's `501` response.
The [wildcard cleanup decision](adr/release-wildcard-interests-through-their-public-uri.md) records the difference and reference cleanup method.
Bridge comparisons cover creation, retrieval, listing, duplicate errors, and deletion.
The test normalizes creation timestamps.
The test checks `video_mode=none` for Rusterisk and `video_mode=talker` for the reference before comparing the remaining fields.
The [audio bridge decision](adr/report-bridge-video-mode-within-supported-media.md) records the media scope difference.

External channel comparisons check bound transport variables and Stasis participation.
The client continues an external channel through the same dialplan on each server and checks the next `StasisStart` event for the selected extension, context, priority, and arguments.
An explicit channel subscription checks `dialplan_app` and the expanded `dialplan_app_data` in `ChannelDialplan` before accepting `StasisStart`.
The five dialed numbers cover literal matching, overlapping digit patterns, early matching, and a jump to another context.
Audio comparisons connect two external channels to a bridge on each server.
Both directions send distinct known samples and check received sample values for `PCMU` and `PCMA`.

The hold music comparison checks looping, `channels.mute`, playback interruption, and resource release.
The first matching audio must arrive within three seconds.
The measurement deadline includes the time needed to receive the required continuous audio.
If the Asterisk packet stream skips sequence numbers and its timestamps advance by the corresponding number of 20 ms packets, the measurement starts again with the next received packet.
The missing packets contribute no samples to the measured interval.
Asterisk suppresses outgoing voice packets while the other channel continues receiving audio.
Rusterisk sends silence on the muted channel.
After channel playback interrupts hold music, the [channel contract](https://docs.asterisk.org/Asterisk_22_Documentation/API_Documentation/Asterisk_REST_Interface/Channels_REST_API/#startmoh) requires an explicit start.
The comparison receives silence from Rusterisk until an explicit start.
On a bridged external media channel, Asterisk 22.9.0 resumes hold music when channel playback ends.
The comparison checks the resumed audio and then stops it explicitly.
Asterisk publishes the final playback state before removing the playback from its [resource container](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_stasis_playback.c).
After receiving `PlaybackFinished`, the client checks that the matching terminal snapshot disappears within three seconds.

Recording comparisons use the Rust client to start channel and bridge recordings on each server.
For bridge recordings, both peers must receive the opposite input before the measured interval starts.
The client waits for `RecordingFinished` before downloading the completed audio.
At least 80 milliseconds of consecutive samples must match the received channel input or bridge mix.
The audio check rejects silence, different samples, brief fragments, and malformed files.
AudioSocket comparisons check binary session identifiers, Stasis participation, bridge membership, and exact received samples in both directions.
Incoming and configured outgoing WebSocket comparisons check the media subprotocol, start metadata, known samples in both directions, and JSON and plain text controls.
Outgoing comparisons also check basic authentication, destination queries, and manual answer.
Control comparisons check buffering, flow thresholds, pause, resume, flushing, queue markers, drained notifications, direction changes, and telephone digits.
Incoming Opus comparisons decode distinct received tones in both directions.
The runner inspects its reference container's private address and requires direct container routing.
Stream callbacks use the reference's resolution of `host.docker.internal` and ephemeral host listener ports.
The local rehearsal used OrbStack on macOS ARM64.

The runner also executes the same blind and attended transfer fixtures with both servers.
Each answering peer checks the dialed number in the Request-URI of the `INVITE` sent to a `PJSIP/number@endpoint` destination.
The attended case changes both surviving signals after confirming retirement of the source dialogs.
The surviving peers exchange `BYE` and `200 OK` before the fixture checks that each server has released all channels.
The [transfer profile](blind-transfer.md) records received-audio requirements and remaining wire differences.

The runner compares authenticated registration and removal on both servers with [the same SIPp scenario](../tests/interop/sipp/registration.xml).
Between registration and removal, SIPp sends a `BYE` for an unknown dialog.
The comparison requires the `481` response recommended by [RFC 3261 section 15.1.2](https://www.rfc-editor.org/rfc/rfc3261.html#section-15.1.2).
SIPp matches the response to the `BYE` transaction by its Via branch.
The runner requires complete native SIPp call statistics before proceeding to the remaining server cases.
The paired browser campaign verifies received tones and completed recordings on both servers with [the same browser app](browser-calls.md).
Broader signaling coverage remains unverified.
The capability response keeps `call_verified=false` until the required call and audio comparisons pass.
