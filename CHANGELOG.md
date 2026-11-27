# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/P4suta/Rusterisk/compare/v0.1.0...v0.2.0) - 2026-11-27

### Calls and upgrades

Each server archive includes `rusterisk` and `rusterisk-cli`.
In the loopback configuration, dial `101` for `alice` or `102` for `bob`.
Extension `100` enters `Stasis(calls)`.
Use `doctor extension` to check a route, `doctor channel` to inspect directional media traffic, and `doctor runtime` to see call slots and capacity refusals.
The authenticated capabilities response lists the phone, trunk, and browser paths verified with Asterisk 22.9.0.

Calls keep their starting routes across configuration reloads.
The recording journal reconciles interrupted operations when the server starts.
Stop the old server and back up the configuration and data directories before opening a schema version 2 database with the new release.
Restore both directories from that backup to return to version 2.

The Rust client requires the matching server release for runtime diagnostics.
Supply `call_slots` when constructing `RuntimeActivity` and handle `Doctor::Runtime` in exhaustive matches on the CLI command.
Open a new call session after `Error::Closed` and retry after capacity becomes available for `Error::Capacity`.
See the [upgrade guide](docs/compatibility.md#upgrade-clients-and-stored-data) for HTTP 503 handling and database compatibility.

### Added

- *(server)* preserve calls under load and recover interrupted recordings ([#116](https://github.com/P4suta/Rusterisk/pull/116))

### Other

- *(server)* [**breaking**] drive calls by events and deadlines ([#112](https://github.com/P4suta/Rusterisk/pull/112))

## [0.1.0](https://github.com/P4suta/Rusterisk/releases/tag/v0.1.0) - 2026-11-21

### Added

- *(cli)* verify returned audio on a nominated call ([#29](https://github.com/P4suta/Rusterisk/pull/29))
- *(cli)* diagnose dialplan routes and directional traffic ([#27](https://github.com/P4suta/Rusterisk/pull/27))
- *(cli)* follow live call events without taking control ([#23](https://github.com/P4suta/Rusterisk/pull/23))
- *(cli)* select named server profiles ([#20](https://github.com/P4suta/Rusterisk/pull/20))
- *(cli)* inspect endpoints and active call legs ([#18](https://github.com/P4suta/Rusterisk/pull/18))
- *(cli)* provide a standalone remote client ([#9](https://github.com/P4suta/Rusterisk/pull/9))

### Other

- *(release)* verify the standalone client package ([#30](https://github.com/P4suta/Rusterisk/pull/30))
