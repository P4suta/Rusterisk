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
