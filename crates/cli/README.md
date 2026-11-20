# Rusterisk CLI

Use `rusterisk-cli` to check connectivity or reload configuration on a running Rusterisk server.

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
