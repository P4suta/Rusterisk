# Browser calls

Run the example from the workspace root with the pinned tools.
The [local configuration](../examples/browser/config/) defines two Session Initiation Protocol (SIP) accounts and permits the page origin `http://127.0.0.1:3000`.
The supplied accounts use `test-password` for local development.

Start the server in one terminal:

```sh
mise x -- cargo run --locked --package rusterisk-server --bin rusterisk -- --config-dir examples/browser/config --data-dir .cache/browser-data
```

Start the [Rust bridge example](../crates/client/examples/browser_bridge.rs) in another terminal:

```sh
RUSTERISK_PASSWORD=test-password RUSTERISK_ENDPOINT=PJSIP/bob-endpoint mise x -- cargo run --locked --package rusterisk-client --example browser_bridge
```

Start the page server in a third terminal:

```sh
mise x -- just browser-example
```

Open `http://127.0.0.1:3000` in two browser profiles.
Use `ws://127.0.0.1:8088/ws` as the server URL.
Connect one profile with `sip:alice@localhost`, authentication username `alice`, and password `test-password`.
Connect the other profile with `sip:bob@localhost`, username `bob`, and the same password.
Keep the optional network settings as `[]`.
Call `sip:100@localhost` from Alice and select Answer in Bob's page.
The bridge example answers Alice, originates Bob, and joins the answered channels.
End call or Disconnect releases the microphone.
Ctrl+C stops each terminal process.

The browser requests microphone access when you select Call or Answer.
After a refusal, change the browser's microphone permission to permit access, then repeat the action.
Use the audio player's Play control if automatic playback needs another interaction.
[Loopback origins](https://www.w3.org/TR/secure-contexts/#is-origin-trustworthy) support local microphone access.
Remote signaling requires a `wss://` URL and the network settings supplied by the server administrator.

`mise x -- just browser-example 3001` selects another local page port.
Update `allowed_origins` in `ari.conf` to the exact page origin and restart the server before connecting.
The example serves the maintained HTML, style sheet, and built JavaScript through the existing Rust HTTP adapter.

`mise x -- just browser-check` installs the lockfile, verifies the pinned signaling declarations, checks types and source rules, runs unit tests, and builds the bundle.
The isolated `just browser` campaign drives the browser through actual server requests and compares received tones and stored recording samples.
The campaign uses its own typed control fixture.
The Rust bridge example has separate HTTP and event lifecycle tests.
The campaign also checks permission recovery, cancellation, keyboard focus, targets, narrow layouts, enlarged text, and spacing.
Failure reports include bounded process and resource diagnostics.
The first call scenario measures elapsed milliseconds from just before Alice selects Call.
Its Playwright attachment records the accepted request to call Bob, both browser answers, and the observed tone at each receiver.

Use the browser image and local Asterisk image ID to run the same scenarios on both servers.
The reference run requires a Unix host with Docker Engine that supports isolated bridge mode.
Use `--dry-run` to inspect the selected images and limits.

```sh
mise x -- just interop browser --run --image "$browser_image" --reference-image "$asterisk_image"
```

Both servers passed on Linux ARM64 with Playwright `1.65.1`, Chromium `159.0.8154.6`, and the `ulaw` codec.
The runs matched 660 Hz at Alice, 440 Hz at Bob, and both test signals in each stored recording.
The tests use loopback signaling and gather Asterisk media candidates on the isolated bridge.
VoiceOver, network address translation (NAT) traversal, and [Traversal Using Relays around NAT](https://www.rfc-editor.org/rfc/rfc8656.html) media relays remain unverified.
