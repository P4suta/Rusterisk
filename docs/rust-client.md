# Rust client

The `rusterisk-client` crate shares its resource and event types with `rusterisk-api`.
Requests use the authoritative operation paths and methods.
HTTP errors preserve their status and the server's message.

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
An `OwnedCall` reserves an identifier before sending the request.
`wait` observes a named `CallMilestone` without consuming the session's event queue.
An answer or Stasis event can arrive before the HTTP acceptance response.
The four milestones keep their independent meanings.

`MediaConnected` observes Rusterisk's `RUSTERISK_MEDIA_CONNECTED` channel variable event.
The milestone means the server received an audio frame.
Checking received audio remains necessary to verify the intended signal.
The automatic notification is a Rusterisk extension.

`wait_completion` returns the terminal snapshot after peer termination, cancellation, or failure.
`cancel` makes the handle terminal at the caller and starts remote cleanup.
Late answers and request responses can't reactivate a cancelled handle.
`close().await` cancels the call and waits for cleanup.
`Session::shutdown().await` cancels every owned call and joins the event driver and HTTP workers.
Cleanup failures reach the caller and the structured diagnostics.

Dropping a call or session initiates cleanup while the Tokio runtime remains available.
Await explicit cleanup before shutting down the runtime.

## Bound transport and event work

A session holds at most 64 call handles and remembers at most 4096 issued identifiers.
The event queue retains 64 events.
`next_event` reports `Error::EventLagged` when the caller falls behind.
Call progress continues to update while the caller waits for a milestone.

Requests have a ten-second deadline and a five-second connection deadline.
The client rejects URLs larger than 4096 bytes and JSON response bodies larger than one mebibyte.
The event transport limits messages to 64 kibibytes.
Session cleanup has a 22-second deadline.
The driver then aborts and joins any remaining workers.

## Download stored audio

Use `stored_recordings` and `stored_recording` to inspect completed recordings.
Use `download_stored_recording` with an asynchronous output writer to stream the stored bytes.
The method returns the number of bytes written and accepts at most one gibibyte plus the 44-byte wave header.
Response headers and error responses have ten-second deadlines.
The transfer has a ten-minute deadline, with five seconds for each read or write.
Dropping the download future releases its response transport.
A failed or cancelled transfer can leave partial output, which the caller must discard or recover.
Use `delete_stored_recording` to remove a completed recording.
