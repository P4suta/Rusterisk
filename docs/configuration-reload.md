# Reload configuration

Edit and save the replacement files in the running server's startup configuration directory, selected with `--config-dir`.
Reload reads the server's fixed startup directory.

Authenticate with a current user who has write access.
Use the password accepted by the running server even when the replacement files change the credentials.
Set `--server` to an HTTPS origin or a loopback HTTP origin.
Keep passwords out of arguments, URLs, and logs.

In a POSIX shell, redirect an existing protected UTF-8 secret file into standard input:

```sh
rusterisk reload --server http://127.0.0.1:8088 --username operator --password-stdin < "/secure/rusterisk-reload-password"
```

The CLI removes one optional trailing `LF` or `CRLF`.
The password must contain 1 to 256 UTF-8 bytes after removal.
The CLI preserves spaces and rejects other line breaks, empty input, and invalid UTF-8 before connecting.
`--password-stdin` requires a pipe or file rather than terminal input.

Only `204 No Content` confirms completed publication.
The CLI then prints `Configuration reloaded.` and exits successfully.
`202 Accepted` reports no completed reload and causes a failure exit.
Configuration validation failures preserve the running settings and report the HTTP status, file, line, and reason.
Correct the reported settings before requesting reload again.
A transport failure leaves publication completion uncertain.

Existing calls keep their starting settings.
New calls use the replacement settings after publication.
Listener changes require a server restart.

Windows uses the same authenticated HTTP operation without console signals.
`POST /rusterisk/configuration/reload` belongs to Rusterisk management, separate from the Asterisk REST Interface (ARI) compatibility catalog.
The [Rust client libraries](rust-sdk.md) provide `Client::reload_configuration`.
