# Rusterisk CLI

Use `rusterisk-cli` to check connectivity, inspect calls and endpoints, or reload configuration on a running Rusterisk server.

Install a published release from crates.io with Cargo.

```sh
cargo install --locked rusterisk-cli
```

To install from a source checkout, run Cargo at the repository root.

```sh
mise x -- cargo install --locked --path crates/cli
```

Pass the server origin, username, and password source for each operation.

```sh
rusterisk-cli ping --server https://pbx.example.org --username operator --password-stdin < password.txt
rusterisk-cli reload --server https://pbx.example.org --username operator --password-stdin < password.txt
```

Supply the current password through a protected file or pipe.
Use HTTPS for remote servers.
Plain HTTP is available on loopback addresses.
A successful ping prints `pong`.
A completed reload prints `Configuration reloaded.`.

## Inspect calls and endpoints

Use a read-only ARI account for inspection.
The `status` command reports the server version and startup time.
The `endpoints` command lists configured routes and `endpoint TECH/RESOURCE` selects one route.
The `channels` command lists active call legs and `channel ID` selects one leg.
Each leg includes the identifiers of bridges that listed the channel during the inspection.
Channel and bridge reads can observe different moments during a call.

```sh
rusterisk-cli endpoint PJSIP/desk --json --server https://pbx.example.org --username reader --password-stdin < password.txt
rusterisk-cli channels --json --server https://pbx.example.org --username reader --password-stdin < password.txt
```

With `--json`, list commands return an array and a lookup returns an object.
An empty array indicates a successful inspection with no matching resources.
Server status includes `version` and `startup_time`.
Endpoint objects include `technology`, `resource`, `routing_state`, `registration`, and `channel_ids`.
The [ARI endpoint model](https://github.com/asterisk/asterisk/blob/22.9.0/rest-api/api-docs/endpoints.json) reports routing availability, so registration is `unknown` for static and dynamic contacts.
Each channel object has `kind` set to `call_leg` and includes `id`, `name`, `state`, `creationtime`, and `bridge_ids`.
A failed request writes a diagnostic to standard error and exits unsuccessfully.

## Watch a call or endpoint

Use `watch channel ID` to follow one channel from its initial snapshot through hangup.
Use `watch endpoint TECH/RESOURCE` to follow changes to a route and its call legs until interrupted.
The read-only observer runs independently of the Stasis application controlling the call.

```sh
rusterisk-cli watch channel inbound-42 --json --profile lab --password-stdin < password.txt
```

Each line of JSON contains a timestamp and a `type` of `snapshot`, `event`, or `gap`.
Channel generations distinguish calls that reuse an identifier.
A gap reports a lost connection or missed records and exits unsuccessfully.
Start another watch to obtain a fresh snapshot.

## Named servers

Store server origins and usernames in `rusterisk/profiles.json` under the [XDG configuration directory](https://specifications.freedesktop.org/basedir/latest/).
An absolute `XDG_CONFIG_HOME` selects the directory.
The default is `$HOME/.config` on Unix and `%APPDATA%` on Windows.

```json
{
  "profiles": [
    {"name": "lab", "server": "http://127.0.0.1:8088", "username": "tester"},
    {"name": "live", "server": "https://pbx.example.org", "username": "reader"}
  ]
}
```

List configured names offline with `rusterisk-cli profiles`.
Select a profile for any remote command and supply the password at invocation.

```sh
rusterisk-cli status --profile lab --password-stdin < password.txt
```

Use either a named profile or the explicit server and username options.
A missing profile, a duplicate name, an invalid origin, or an unknown configuration field causes profile selection to fail before connecting.
