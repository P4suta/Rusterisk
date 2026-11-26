# Server candidate packages

Each candidate contains the `rusterisk` server, the `rusterisk-cli` client, configuration examples, and both license texts.
Unix candidates use `tar.gz` and Windows candidates use `zip`.
Unpack a candidate into an empty directory and run commands from its root.

## Make the first call

Choose a password for the `api` account in `config/ari.conf`.
Choose a separate password for each phone account in `config/pjsip.conf`.
The sample HTTP and Session Initiation Protocol (SIP) listeners bind to loopback.
Run both phones on the same computer as the server.

On Linux and macOS, check both versions and validate the configuration before starting the server.

```sh
./bin/rusterisk --version
./bin/rusterisk-cli --version
./bin/rusterisk --check-config --config-dir config
./bin/rusterisk --config-dir config --data-dir data
```

On Windows, use the executable with the `.exe` extension.

```powershell
.\bin\rusterisk.exe --version
.\bin\rusterisk-cli.exe --version
.\bin\rusterisk.exe --check-config --config-dir config
.\bin\rusterisk.exe --config-dir config --data-dir data
```

Register two softphones using SIP over User Datagram Protocol (UDP) at `127.0.0.1:5060`.
Use `alice` and `bob` as their respective SIP usernames and authentication IDs.
Enter the matching passwords from `config/pjsip.conf` and enable G.711 μ-law on both phones.
Dial `102` on Alice's phone, answer on Bob's phone, and speak in each direction.
Hang up and dial `101` from Bob's phone to reach Alice.
Extension `100` enters the Stasis app `calls`.

For local diagnostics, save the Asterisk REST Interface (ARI) password for `api` in a file readable only by your account.
Open another terminal to check the destination and its registration before dialing.

```sh
./bin/rusterisk-cli doctor extension internal 102 --server http://127.0.0.1:8088 --username api --password-stdin < ari-password.txt
```

For an answered call with no sound, list the active channels and substitute the affected channel identifier for `CHANNEL_ID`.
The runtime diagnosis reports the occupied slots, slots awaiting media release, configured limit, and capacity refusals.

```sh
./bin/rusterisk-cli channels --server http://127.0.0.1:8088 --username api --password-stdin < ari-password.txt
./bin/rusterisk-cli doctor channel CHANNEL_ID --server http://127.0.0.1:8088 --username api --password-stdin < ari-password.txt
./bin/rusterisk-cli doctor runtime --server http://127.0.0.1:8088 --username api --password-stdin < ari-password.txt
```

In PowerShell, pipe the password file to the packaged client.

```powershell
Get-Content -Raw .\ari-password.txt | .\bin\rusterisk-cli.exe doctor extension internal 102 --server http://127.0.0.1:8088 --username api --password-stdin
```

## Upgrade or restore a backup

Stop the server and back up the entire `config` directory and the entire directory selected by `--data-dir`.
Keep the SQLite database, recording files, and pending recording stages together.
Unpack the new package into a fresh directory.
Copy the backed-up configuration and data into the directory.
Validate the copied configuration with `--check-config`.
Start the new server with the copied data.
Startup reconciles interrupted calls and recordings before serving requests.
To restore a backup, stop the server and start the matching package with a fresh copy of both backed-up directories.

## Verify a candidate

Candidate verification checks archive contents and executable headers before native startup and API tests.
Header checks establish the declared format and processor.
The checks follow the [Executable and Linkable Format header](https://gabi.xinuos.com/elf/02-eheader.html), [Mach-O header](https://github.com/apple-oss-distributions/xnu/blob/main/EXTERNAL_HEADERS/mach-o/loader.h), and [Portable Executable header](https://learn.microsoft.com/en-us/windows/win32/debug/pe-format) definitions.
Native execution and dependency checks establish runtime compatibility for the selected candidate.

Build and inspect a candidate with the maintained distribution commands.

```sh
mise x -- just distribution-server-build aarch64-apple-darwin
mise x -- just distribution-server-check-content <candidate-directory> aarch64-apple-darwin
mise x -- just distribution-server-check-startup <candidate-directory>
```

The startup check selects the host operating system and processor.
It checks version output and the packaged configuration before starting an extracted executable.
On Linux and macOS, it also checks system library resolution before startup.
On Windows, it checks loaded dynamic-link library paths while the extracted executable runs.
A separate configuration copy changes only the sample HTTP and SIP listener ports.
The runner retains bounded command logs under `.cache/distributions/server-startup`.
It requires a complete readiness record, graceful shutdown, and both listener ports to become available.
It removes the owned extraction and runtime data after the check.
The software development kit (SDK) check connects an independent client to the packaged server.
Use `--consumer-jobs` to control the Cargo jobs for client preparation.
The default is two jobs.
The release gate uses one Cargo job for the SDK consumer on Windows.

```sh
mise x -- cargo run --locked --target-dir target/xtask --package xtask -- distribution server verify --candidate "$server_candidate" --sdk-candidate "$sdk_candidate" --consumer-jobs 1
```

To exercise a native archive with two phones, set `RUSTERISK_PACKAGED_ROOT` to its extracted directory.
To exercise a database upgrade, point `RUSTERISK_BASELINE_SERVER` at a server with schema version 2 and `RUSTERISK_CANDIDATE_SERVER` at the extracted candidate executable.
The checks create temporary settings and data directories and use loopback ports.

```sh
cargo test --locked -p rusterisk-server --test sip_call the_packaged_phones -- --ignored
cargo test --locked -p rusterisk-server --test process_recovery the_candidate_upgrades -- --ignored
```

On Apple silicon, request [Rosetta execution](https://developer.apple.com/documentation/apple-silicon/building-a-universal-macos-binary) explicitly when checking a macOS Intel candidate.

```sh
mise x -- just distribution-server-build x86_64-apple-darwin
mise x -- just distribution-server-check-content <candidate-directory> x86_64-apple-darwin
mise x -- just distribution-server-check-startup-rosetta <candidate-directory>
```

The runner reports `execution=rosetta` and rejects unsupported host combinations.
Translated startup doesn't establish native Intel performance.
Use `distribution-server-check-startup-target <candidate-directory> <target>` to select another native target explicitly.

The native plan and build commands accept a Cargo job count and a build timeout in seconds.
The defaults are two Cargo jobs and a 900-second build timeout.
The following commands use one Cargo job and a build timeout of 1,800 seconds.

```powershell
mise x -- just distribution-server-plan x86_64-pc-windows-msvc 1 1800
mise x -- just distribution-server-build x86_64-pc-windows-msvc 1 1800
```
