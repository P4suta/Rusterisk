# Asterisk compatibility

Rusterisk targets the public contracts in [Asterisk 22.9.0](https://github.com/asterisk/asterisk/tree/22.9.0/rest-api/api-docs).
The public Rust models and operation catalog generate the discovery documents and OpenAPI description.
Declaring an operation in discovery doesn't certify its call behavior.
The authenticated `/rusterisk/capabilities` endpoint reports the reference version and each operation's `call_verified` value.
Every operation reports `false` while complete operation coverage remains pending.

## Control interface

The server provides authenticated channel creation, origination, answering, ringing, hangup, variables, hold, directional audio controls, and telephone digits.
Channels can enter a subscribed Stasis app, move between subscribed apps, or continue in a configured dialplan.
Leaving Stasis releases the Rust client's control handle without ending the server's channel.
Bridge creation, membership changes, deletion, channel playback, bridge playback, and recording use the current resource generation.
Audio bridges report `video_mode=none` because the approved initial scope defers video.
The reference reports `talker` for a default mixing bridge.
Stored recordings support listing, metadata, copying, deletion, and streamed `audio/wav` downloads.
The ping endpoint returns a `pong` response.
App queries report subscribed apps and their channel and bridge resources.
Event filters support partial updates, rejection precedence, and reset through an empty request.
Explicit subscriptions observe channels, bridges, configured endpoints, or all resources of one kind.
Endpoint technology subscriptions also observe their channels.
All-endpoint subscriptions report endpoint state changes.
Subscription lists process sources in order and preserve earlier successful entries when a later source fails.
Repeated specific subscriptions require matching removals.
Resource deletion clears specific subscriptions before identifier reuse.
Rusterisk supports [wildcard removal](adr/release-wildcard-interests-through-their-public-uri.md), while the reference rejects removal through the empty bridge identifier.
Endpoint queries report configured resources, routing availability, and exact channel membership.
Registration expiry changes routing availability without ending active calls.

Known operations without working adapters return `501`.
Channel redirection still uses that response.
Sound queries list configured wave files by sound identifier, language, and format.
Channel and bridge playback resolve sound identifiers within the configured library and preserve each request's language.
Authenticated readers can select [server information](server-information.md) sections with case-insensitive names.
The response preserves process identity, startup time, and the bounded channel limit.
Registered JSON body parameters override query values while path identifiers remain authoritative.
Body values follow the public operation's declared types and bounded conversion rules.
Channel creation validates initial variables together and applies them before publishing creation events.
Origination accepts an explicit channel identifier in either the query or the resource path.
The path identifier takes precedence over query and JSON fields.
Caller ID parsing separates the display name and number before applying initial variables.
Playback provides start, pause, resume, stop, restart, forward, and reverse.
Forward and reverse use a three-second step from the current playback position by default.
Playback requests accept an initial `offsetms` and a `skipms` step between zero and 2,147,483,647 milliseconds.
A zero `skipms` selects the default step.
Paused and stopping playbacks reject position changes.
Queued restart preserves the original request.
The same Rust client compares channel and bridge position changes with Asterisk 22.9.0 using received audio.
The comparison also checks an initial seven-second offset, one-second position changes, paused-state rejection, resume, stop, and completion events.
Restart returns to the beginning even when the request supplied an initial offset.
Media lists remain unsupported.
Live recording duration statistics remain absent.

## Signaling and audio

The signaling adapter handles `SIP` over `UDP`, native `TCP`, verified `TLS`, and signaling WebSockets, with digest authentication, incoming calls, outgoing calls, cancellation, and hangup.
The [inbound authentication profile](inbound-sip-authentication.md) defines configured Digest algorithms, prepared identities, nonce boundaries, and initial INVITE requirements.
Native stream tests cover authenticated registration, partial messages, shared flows, initial outbound connections, call control, half-close responses, saturation, and shutdown.
HTTP `/ws` upgrades require the `sip` subprotocol and enforce the configured Origin policy.
Authenticated REGISTER requests select an address of record through the endpoint configuration and the To URI.
Each address has a configurable dynamic binding limit capped at 16, within a shared limit of 128 per signaling engine.
A zero contact limit disables dynamic registration.
Binding updates apply atomically, and successful responses report the remaining bindings.
Each Contact's `expires` value overrides `Expires`.
For omitted expiry, `default_expiration` supplies the requested lifetime.
`minimum_expiration` and `maximum_expiration` bound positive lifetimes.
Queries, targeted removal, wildcard removal, and expiry preserve unrelated bindings.
Closing a registered stream removes its bindings while preserving other peers.
Endpoint availability changes when its first binding appears or last binding disappears across configured addresses.
Calls select live bindings by configured address order, then Contact preference.
Bindings keep Contact URIs separate from physical routing peers and route browser placeholder hosts through the registered connection.
Outbound `REGISTER` clients support authenticated startup, Contact grants, refresh, `Min-Expires` corrections, and bounded retries.
The [startup profile](outbound-registration-startup.md) describes connection reuse and remaining alternate-server and configuration replacement work.
Complete URI header matching remains pending.
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
Native `DTLS-SRTP` session tests exchange known audio with `PCMU`, `PCMA`, and Opus.
A `PCMU` integration test checks both directions through the audio bridge and port release.
Blind and attended transfer comparisons verify received audio with Asterisk 22.9.0, including fresh signals after retirement of the source dialog.
The [transfer profile](blind-transfer.md) records remaining authorization, topology, and termination-timing boundaries.
Legacy hold through an unspecified connection address remains pending.
External media supports bidirectional unicast datagrams with numeric destinations and `ulaw` or `alaw` formats.
Creation returns bound transport variables after socket admission.
Stasis participation and received audio remain independent milestones.
The Rust client exposes typed external datagram targets and bounded bridge membership operations.
AudioSocket supports client connections with numeric destinations, binary session identifiers, and 8 kHz signed linear audio through `format=slin`.
Connection attempts have deadlines and preserve signaling progress.
Cancelling admission closes the owned connection without reviving the channel.
Incoming external WebSocket channels publish an ephemeral connection identifier and an optimal frame size.
Media connections use the connection identifier as a bearer credential.
An explicit `Sec-WebSocket-Protocol` offer must include `media`.
Connections without an offer receive no unsolicited subprotocol header.
Origin validation and exclusive connection claims protect channel generations.
The service supports JSON and plain text controls, G.711 and signed linear audio, buffering, queue markers, pause, resume, and direction changes.
Direction changes require JSON controls.
Disconnecting releases the channel and its owned media.
Configured outgoing WebSocket clients preserve destination queries, negotiate the media subprotocol, and support basic authentication.
Encrypted connections verify the certificate and peer name through native trust roots or a configured certificate list.
Owned connection jobs enforce configured retry limits and a four-second admission deadline.
Cancellation releases pending connections and prevents later retry attempts.
Native and Asterisk reference scenarios decode known tones in both directions over incoming Opus WebSocket connections.

## Configuration and persistence

The server reads `http.conf`, `ari.conf`, `pjsip.conf`, `rtp.conf`, and `extensions.conf` with file and line diagnostics.
Optional `chan_websocket.conf` and `websocket_client.conf` files configure media controls and outgoing media clients.
Unsupported configuration options fail validation.
Dialplan execution covers `NoOp`, `Set`, `Answer`, `Dial`, `Stasis`, `Playback`, `Wait`, `Goto`, and `Hangup`.
`Wait` accepts nonnegative decimal durations, including fractions, exponents, and the `ms`, `s`, `m`, and `h` units in the [reference time parser](https://github.com/asterisk/asterisk/blob/22.9.0/main/app.c#L3106-L3172).
Durations use integer milliseconds and discard fractional milliseconds.
Values greater than 2,147,483,647 milliseconds fail configuration validation.
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
The private historical JavaScript fixture uses a [scoped dependency assessment](legacy-client-dependency-scope.md) that reports its known advisories and rejects unreviewed changes.
Datagram tests compare received audio through actual sockets and bridges, including peer hold, resume, and mixed packet durations.
The local `just interop` gate compares bridge lifecycle, endpoint resources, routing availability, app subscriptions, external channel admission, and resource errors with Asterisk 22.9.0.
Both servers bridge known external audio samples in both directions with `PCMU` and `PCMA`.
AudioSocket comparisons also check session identifiers and exact received samples through both servers.
Incoming and configured outgoing WebSocket comparisons check start and status messages and known received samples in both directions with `PCMU`.
Both control formats also compare buffering, flow control, queue markers, pause, resume, manual answer, and telephone digit notifications.
The reference distinguishes case-sensitive technology lists from case-insensitive endpoint detail queries.
See [reference comparisons](reference-comparisons.md) for scope and resource limits.
The native browser campaign verifies received audio and stored recordings on Linux ARM64 loopback.
Broader reference signaling calls, browser calls with Asterisk, supported-platform distributions, and physical `HT813` observations remain unverified.
