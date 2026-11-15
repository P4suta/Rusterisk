# Asterisk compatibility

Rusterisk targets the public contracts in [Asterisk 22.9.0](https://github.com/asterisk/asterisk/tree/22.9.0/rest-api/api-docs).
The public Rust models and operation catalog generate the discovery documents and OpenAPI description.
Declaring an operation in discovery doesn't certify its call behavior.
The authenticated `/rusterisk/capabilities` endpoint reports the reference version and each operation's `call_verified` value.
Every operation reports `false` because reference call and received-audio comparisons haven't run.

## Control interface

The server provides authenticated channel creation, origination, answering, ringing, hangup, variables, hold, directional audio controls, and telephone digits.
Channels can enter a subscribed Stasis app, move between subscribed apps, or continue in a configured dialplan.
Leaving Stasis releases the Rust client's control handle without ending the server's channel.
Bridge creation, membership changes, deletion, channel playback, bridge playback, and recording use the current resource generation.
Stored recordings support listing, metadata, copying, deletion, and streamed `audio/wav` downloads.
The ping endpoint returns a `pong` response.
App queries report subscribed apps and their channel and bridge resources.
Endpoint queries report configured resources, routing availability, and exact channel membership.
Registration expiry changes routing availability without ending active calls.

Known operations without working adapters return `501`.
Explicit resource subscriptions, sound queries, server information, external media, and channel redirection still use that response.
Requests with JSON bodies also return `501` after body validation.
Playback supports start, pause, resume, and stop.
Seeking and media lists remain unsupported.
Live recording duration statistics remain absent.

## Signaling and audio

The signaling adapter handles `SIP` over `UDP`, digest authentication, one registered contact per endpoint, incoming calls, outgoing calls, cancellation, and hangup.
Authenticated endpoint identity determines the context independently of displayed caller identification.
Telephone digits use `RFC 4733` packets or `SIP INFO` according to the endpoint's configuration.
The audio adapter decodes `PCMU`, `PCMA`, and Opus into complete 20 ms mixer frames.
G.711 packet durations can reach 200 ms, and Opus packet durations can reach 120 ms within the 2048-byte datagram bound.

In-dialog offers can change media direction while preserving codec, payload mappings, connection address, port, and other media attributes.
Changed descriptions require an unchanged session identity and a session version increment of one.
Rejected offers preserve the active media session.
Peer hold and resume preserve local hold and directional audio controls.
The adapter confirms direction changes before sending the negotiated answer.
Unsupported media changes return `488`.
Blind transfer, attended transfer, stream transports, browser media, and legacy hold through an unspecified connection address remain pending.
The AudioSocket codec and secure media primitives don't yet provide external media services.

## Configuration and persistence

The server reads `http.conf`, `ari.conf`, `pjsip.conf`, `rtp.conf`, and `extensions.conf` with file and line diagnostics.
Unsupported configuration options fail validation.
Dialplan execution covers `NoOp`, `Set`, `Answer`, `Dial`, `Stasis`, `Playback`, `Wait`, `Goto`, and `Hangup`.
Parallel `PJSIP` destinations use a bounded set of branches and select the first answered peer.
Reload comparison validates replacement settings.
The running server doesn't apply replacement settings.
Waiting connections and queue admission have explicit limits.

An owned storage worker writes call history and recording files.
Restart recovery marks previous active calls interrupted and repairs incomplete recording files.
Recording replacement, append, and copying preserve existing completed files until publication.
Forced process termination and storage fault campaigns still require integration verification.

## Verification status

The local `just check` gate covers formatting, source policy, Rust 1.89 core tests, strict Clippy, type checking, workspace tests, and generated interface contracts.
The local `just security` gate checks advisories, dependency policy, licenses, sources, and secrets.
Datagram tests compare received audio through actual sockets and bridges, including peer hold, resume, and mixed packet durations.
The local `just interop` gate compares endpoint resources, routing availability, app subscriptions, and resource errors with Asterisk 22.9.0.
The reference distinguishes case-sensitive technology lists from case-insensitive endpoint detail queries.
See [reference comparisons](reference-comparisons.md) for scope and resource limits.
Reference calls, browser audio, supported-platform distributions, and physical `HT813` observations remain unverified.
