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
Sound queries, server information, and channel redirection still use that response.
Registered JSON body parameters override query values while path identifiers remain authoritative.
Body values follow the public operation's declared types and bounded conversion rules.
Channel creation validates initial variables together and applies them before publishing creation events.
Playback supports start, pause, resume, and stop.
Seeking and media lists remain unsupported.
Live recording duration statistics remain absent.

## Signaling and audio

The signaling adapter handles `SIP` over `UDP`, native `TCP`, and verified `TLS`, digest authentication, one registered contact per endpoint, incoming calls, outgoing calls, cancellation, and hangup.
Native stream tests cover authenticated registration, partial messages, shared flows, initial outbound connections, call control, half-close responses, saturation, and shutdown.
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
Blind transfer, attended transfer, WebSocket signaling, browser media, and legacy hold through an unspecified connection address remain pending.
External media supports bidirectional unicast datagrams with numeric destinations and `ulaw` or `alaw` formats.
Creation returns bound transport variables after socket admission.
Stasis participation and received audio remain independent milestones.
The Rust client exposes typed external datagram targets and bounded bridge membership operations.
AudioSocket supports client connections with numeric destinations, binary session identifiers, and 8 kHz signed linear audio through `format=slin`.
Connection attempts have deadlines and preserve signaling progress.
Cancelling admission closes the owned connection without reviving the channel.
Incoming external WebSocket channels publish an ephemeral connection identifier and an optimal frame size.
Media connections use the `media` subprotocol and the connection identifier as a bearer credential.
Origin validation and exclusive connection claims protect channel generations.
The service supports JSON and plain text controls, G.711 and signed linear audio, buffering, queue markers, pause, resume, and direction changes.
Direction changes require JSON controls.
Disconnecting releases the channel and its owned media.
Configured outgoing WebSocket clients preserve destination queries, negotiate the media subprotocol, and support basic authentication.
Encrypted connections verify the certificate and peer name through native trust roots or a configured certificate list.
Owned connection jobs enforce configured retry limits and a four-second admission deadline.
Cancellation releases pending connections and prevents later retry attempts.
WebSocket media with Opus remains pending.

## Configuration and persistence

The server reads `http.conf`, `ari.conf`, `pjsip.conf`, `rtp.conf`, and `extensions.conf` with file and line diagnostics.
Optional `chan_websocket.conf` and `websocket_client.conf` files configure media controls and outgoing media clients.
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
The private historical JavaScript fixture uses a [scoped dependency assessment](legacy-client-dependency-scope.md) that reports its known advisories and rejects unreviewed changes.
Datagram tests compare received audio through actual sockets and bridges, including peer hold, resume, and mixed packet durations.
The local `just interop` gate compares bridge lifecycle, endpoint resources, routing availability, app subscriptions, external channel admission, and resource errors with Asterisk 22.9.0.
Both servers bridge known external audio samples in both directions with `PCMU` and `PCMA`.
AudioSocket comparisons also check session identifiers and exact received samples through both servers.
Incoming and configured outgoing WebSocket comparisons check start and status messages and known received samples in both directions with `PCMU`.
Both control formats also compare buffering, flow control, queue markers, pause, resume, manual answer, and telephone digit notifications.
The reference distinguishes case-sensitive technology lists from case-insensitive endpoint detail queries.
See [reference comparisons](reference-comparisons.md) for scope and resource limits.
Reference signaling calls, browser audio, supported-platform distributions, and physical `HT813` observations remain unverified.
