# Rust client

The `rusterisk-client` crate shares its resource and event types with `rusterisk-api`.
Requests use the authoritative operation paths and methods.
HTTP errors preserve their status and the server's message.
`applications` and `application` read subscribed apps and their channel and bridge resources.
`endpoints`, `endpoints_by_technology`, and `endpoint` read configured endpoint resources and routing availability.

## Run the call example

Start the server with the bundled configuration and register the `alice` telephone.
Set `RUSTERISK_PASSWORD` to the password configured for the `api` user.
Run the example from the workspace root:

```sh
mise x -- cargo run --locked --package rusterisk-client --example call
```

The example subscribes to the `calls` app before originating `PJSIP/alice`.
Answer the telephone to reach `Stasis` and `Answered`.
Send audio from the telephone to reach `MediaConnected`.
Press Ctrl+C to cancel during setup or end an active call.
The example waits for channel cleanup and session shutdown before returning.

`RUSTERISK_URL` selects the server origin and defaults to `http://127.0.0.1:8088`.
`RUSTERISK_USERNAME` defaults to `api`.
`RUSTERISK_APP` defaults to `calls`.
`RUSTERISK_ENDPOINT` defaults to `PJSIP/alice`.
External origins require HTTPS.

## Own calls and observe progress

Create a `Client`, await `subscribe`, and call `Session::originate` with `Originate` options.
Set `Originate::variables` before calling `originate` to send initial channel variables in a bounded JSON body.
The server installs valid values before publishing the channel's creation event.
Empty variable names, excess entries, and oversize bodies fail before the client starts a request.
An `OwnedCall` reserves an identifier before sending the request.
`wait` observes a named `CallMilestone` without consuming the session's event queue.
An answer or Stasis event can arrive before the HTTP acceptance response.
The four milestones keep their independent meanings.

Use `Session::adopt` after receiving an incoming channel's `StasisStart` event.
The session retains at most 64 live incoming observations and preserves answer and media events received before adoption.
Adoption requires an observation in the session's app and rejects unknown, departed, and claimed identifiers.
An adopted call has no HTTP acceptance milestone.
Wait for each of `Stasis`, `Answered`, and `MediaConnected`, and close the handle or shut down the session to end a call that remains under client control.
A destination session can adopt the channel after observing its own `StasisStart` event following an app move.
Use `Client::move_channel` to select a subscribed destination app and its arguments.
Supply at most 32 nonempty arguments, each limited to 256 bytes and containing no commas or control characters.
The client sends up to 4096 bytes of joined arguments in the JSON request body.

Use `Client::continue_in_dialplan` to return a channel to its dialplan.
Omit the context, extension, and priority to continue after the current step.
Context and extension values accept at most 128 bytes without control characters.
Priorities range from 0 through 65535, with zero selecting the server's default position.

Use `Client::ring` and `Client::stop_ringing` for ringing requests.
Both methods preserve the selected server's HTTP errors.
Rusterisk returns `501` for `Client::stop_ringing`.

`MediaConnected` observes Rusterisk's `RUSTERISK_MEDIA_CONNECTED` channel variable event.
The milestone means the server received an audio frame.
Checking received audio remains necessary to verify the intended signal.
The automatic notification is a Rusterisk extension.
After Stasis entry, an independent worker also reads the current media connection variable.
A missing variable leaves the milestone pending for a later notification.
Other request failures appear in `CallSnapshot::media_probe_failure` and return `Error::MediaProbe` from the media milestone wait.
A later connection notification clears the failure and completes the milestone.

`wait_completion` returns the terminal snapshot after peer termination, cancellation, or failure.
Leaving Stasis completes the handle with `CallCompletion::StasisExited` and releases remote cleanup responsibility.
The result identifies released client control without asserting that the channel ended.
Closing a released handle or its session preserves the channel in its destination app or dialplan.
Milestone waits return `Error::StasisExited` after the channel leaves client control.
`cancel` makes the handle terminal at the caller and starts remote cleanup.
Late answers and request responses can't reactivate a cancelled handle.
`close().await` cancels the call and waits for cleanup.
`Session::shutdown().await` cancels every owned call and joins the event driver and HTTP workers.
Cleanup failures reach the caller and the structured diagnostics.

Dropping a call or session initiates cleanup while the Tokio runtime remains available.
Await explicit cleanup before shutting down the runtime.

## Exchange external audio

Use `Session::external_websocket` with `WebSocketMediaOptions` to create an owned incoming media channel after event subscription.
Wait for `Accepted`, read its channel snapshot, and pass the channel to `Client::connect_media` with the selected `MediaControlFormat`.
The connection uses the channel's bearer identifier and negotiates the `media` subprotocol.
The method checks the initial `MEDIA_START` channel and connection identifiers before returning.
`start_event` exposes the negotiated format and frame size.

Send encoded audio with `MediaSession::send_audio` and typed controls with `command`.
`next_message` returns `MediaMessage::Audio` or a parsed `MediaMessage::Event`.
Binary messages carry at most 65500 bytes and commands carry at most 128 bytes.
Malformed events and events for another channel close the connection.
Writes have a 100-millisecond deadline.
Cancelling a write closes the socket and prevents reuse of a partial command.
A pending read preserves its partial message when cancelled.

`MediaSession::close` waits up to two seconds for the peer's close response.
Dropping the media session releases its socket immediately.
Closing the owned call also ends its media socket at the server.
Native integration tests compare known audio received in both directions through two owned channels and verify channel and socket cleanup.

Use `Client::external_websocket_client` to request an outgoing connection through a configured `websocket_client.conf` entry.
Pass the configured connection name as `connection` and select media options explicitly.
The server owns the outgoing socket and retries within its configured admission deadline.

## Run the external audio example

Create a mixing bridge with `create_bridge` and add the intended telephone channel with `add_channels`.
Set `RUSTERISK_BRIDGE` to the bridge identifier and use the same connection settings as the call example.
Run the example from the workspace root:

```sh
mise x -- cargo run --locked --package rusterisk-client --example external_audio
```

The example creates an owned external channel and joins the existing bridge.
It sends a 440 Hz tone as signed little-endian 16-bit samples at 16 kHz in 20-millisecond frames.
It receives audio on the same socket and prints the received byte count once per second.
Press Ctrl+C to end the external channel and release the socket.
The bridge and its other channels stay available.
The example also ends after one hour.

## Bound transport and event work

A session holds at most 64 call handles and remembers at most 4096 issued identifiers.
At most eight originate and media state requests run together, with 16 request workers available for setup and cleanup.
Foreground saturation rejects new originate commands with `Error::Capacity`.
Pending cleanup stays in its call entry until a worker becomes available.
The event queue retains 64 events.
`next_event` reports `Error::EventLagged` when the caller falls behind.
Call progress continues to update while the caller waits for a milestone.

Requests have a ten-second deadline and a five-second connection deadline.
The client rejects URLs larger than 4096 bytes and JSON response bodies larger than one mebibyte.
The event transport limits messages to 64 kibibytes.
Remote cleanup has a 60-second deadline for queued requests and late originate responses.
Closing the event socket has a separate two-second deadline.
The driver then aborts and joins any remaining workers.

## Download stored audio

Use `record_channel` or `record_bridge` with a recording name, format, and `RecordingOptions` to start recording.
The typed options select duration, silence detection, conflict handling, the start prompt, and the termination digit.
The methods return a `LiveRecording`.
Request acceptance doesn't certify recorded audio.
As defined by the [reference recording contract](https://github.com/asterisk/asterisk/blob/22.9.0/rest-api/api-docs/channels.json), channel recording captures received audio, while bridge recording captures the bridge mix.
Use `live_recording` to inspect recording state, `pause_recording` and `resume_recording` to control progress, and `mute_recording` and `unmute_recording` to control captured input.
Use `stop_recording` to request completion and publication.
Use `cancel_recording` to request removal of the live recording.
Observe `RecordingFinished` before reading the completed file.

Use `stored_recordings` and `stored_recording` to inspect completed recordings.
Use `copy_stored_recording` to copy completed audio to an unused recording name.
Use `download_stored_recording` with an asynchronous output writer to stream the stored bytes.
The method returns the number of bytes written and accepts at most one gibibyte plus the 44-byte wave header.
Response headers and error responses have ten-second deadlines.
The transfer has a ten-minute deadline, with five seconds for each read or write.
Dropping the download future releases its response transport.
A failed or cancelled transfer can leave partial output, which the caller must discard or recover.
Use `delete_stored_recording` to remove a completed recording.
