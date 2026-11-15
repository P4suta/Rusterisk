# Server information

Authenticated readers can query `/ari/asterisk/info`.
The optional `only` query accepts comma-separated `build`, `system`, `config`, and `status` sections.
Matching ignores ASCII case and duplicate names.
Unknown names select no section, matching [Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/res/ari/resource_asterisk.c#L273-L369).

The build section reports the binary's target operating system and architecture.
Kernel, build options, build date, and build user remain empty because the build doesn't capture those properties.
The system section reports the package version and a generated process identifier.
Controller clones share the identifier and startup timestamp.
The configured channel limit matches the control task's bounded capacity.
File and load limits are absent because the server doesn't configure those limits.
The default language is `en`, and configured execution user and group remain empty.
The last reload time equals startup time until the server supports runtime configuration reload.

The Rust client uses `Client::server_info` with typed `InfoSection` values.
An empty selection requests all sections.
