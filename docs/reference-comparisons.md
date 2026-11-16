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
The test command has a five-minute deadline.
The runner collects reference logs after a test failure and removes its container after every result.
Each run copies the reference configuration into an owned, bounded temporary directory.
The outgoing media comparison writes its ephemeral callback address into the copy and reloads the reference module.
The runner removes the configuration copy after every result.

The same Rust client queries both servers.
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
Audio comparisons connect two external channels to a bridge on each server.
Both directions send distinct known samples and check received sample values for `PCMU` and `PCMA`.
Recording comparisons use the Rust client to start channel and bridge recordings on each server.
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
The attended case changes both surviving signals after confirming retirement of the source dialogs.
The [transfer profile](blind-transfer.md) records received-audio requirements and remaining wire differences.

The scenario doesn't certify complete operation compatibility.
SIPp scenarios, broader signaling coverage, and browser comparisons still require integration work.
The capability response keeps `call_verified=false` until the required call and audio comparisons pass.
