# Server candidate packages

Each candidate contains the `rusterisk` executable, configuration examples, and both license texts.
Unix candidates use `tar.gz` and Windows candidates use `zip`.
Unpack a candidate into an empty directory and run commands from its root.

On Linux and macOS, inspect the version and validate the configuration before starting the server.

```sh
./bin/rusterisk --version
./bin/rusterisk --check-config --config-dir config
./bin/rusterisk --config-dir config --data-dir data
```

On Windows, use the executable with the `.exe` extension.

```powershell
.\bin\rusterisk.exe --version
.\bin\rusterisk.exe --check-config --config-dir config
.\bin\rusterisk.exe --config-dir config --data-dir data
```

Replace the example Asterisk REST Interface (ARI) password in `config/ari.conf` before accepting client connections.
Replace the example Session Initiation Protocol (SIP) passwords in `config/pjsip.conf`.
The examples bind HTTP and SIP to loopback.
The default dialplan sends extension `100` to the Stasis app `calls`.
Call history and recordings use the directory selected by `--data-dir`.
Keep that directory when replacing the executable.

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

```sh
mise x -- cargo run --locked --target-dir target/xtask --package xtask -- distribution server verify --candidate "$server_candidate" --sdk-candidate "$sdk_candidate" --consumer-jobs 1
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
