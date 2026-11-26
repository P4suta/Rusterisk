# Rust client libraries

Rusterisk provides three Rust libraries in the source checkout.
Install the [remote CLI](../crates/cli/README.md) to control a running server from the terminal.
`rusterisk-core` defines bounded call state and ownership transitions without runtime effects.
The crate uses `no_std`, has no dependencies, and supports Rust 1.89.
`rusterisk-api` defines Asterisk REST Interface (ARI) resources, operation contracts, and events.
`rusterisk-client` sends typed HTTP requests and owns call and event lifetimes.
The API and client packages require Rust 1.99.

Use `ChannelId`, `BridgeId`, `PlaybackId`, and `RecordingName` from `rusterisk_client` when calling typed operations.
Construct each identifier with `try_from`, or reuse the identifier returned by the corresponding resource.
The client reexports the ARI wire models through `rusterisk_client::api`.
A server is a separate runtime dependency.
Operation availability depends on the selected server and its configuration.
The [Rusterisk repository](https://github.com/P4suta/Rusterisk) contains the compatibility contract, configuration examples, and integration fixtures.

Use an asynchronous runtime to send a typed request:

```rust
use rusterisk_client::{Client, Error};

pub async fn check_server(
    address: &str,
    username: &str,
    password: &str,
) -> Result<(), Error> {
    let client = Client::new(address, username, password)?;
    client.ping().await?;
    Ok(())
}
```

Pass credentials at runtime and keep credentials out of logs.
Subscribe to the app before creating an owned call.
Observe request acceptance, Stasis entry, answer, and media connection as independent milestones.
A media connection notification reports a received frame.
Compare received audio to establish the intended signal.

Await `OwnedCall::close` to end a call under client control and wait for remote cleanup.
Await `Session::shutdown` before stopping the asynchronous runtime.
Leaving Stasis releases client control and preserves the channel in its destination app or dialplan.
Dropping a handle starts cleanup while the runtime remains available.
Explicit cleanup reports failures to the caller.

`Client::reload_configuration` reloads the server's fixed startup configuration directory through a separate authenticated management operation.
Supply the running server's current credentials even when the replacement configuration changes the password.
The client sends an empty `POST /rusterisk/configuration/reload` request and accepts only `204 No Content` as completed publication.
The request has a 35-second timeout while ordinary client requests keep their 10-second timeout.
Configuration failures preserve their file, line, and reason in `ApiError.message`.
An embedded server without a startup configuration directory returns `501 Not Implemented`.
Existing calls keep their starting settings, and listener changes require a restart.
The server cancels disconnected requests before the first settings swap.
After the first swap, the server completes publication or stops the service.
A transport failure alone doesn't establish whether publication completed.

The packages use `MIT OR Apache-2.0`.
Each package includes both license texts.
