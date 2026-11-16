# Recording recovery

Server startup locks the storage directory and marks active call records from an earlier run as interrupted.
Startup repairs supported partial wave files and publishes recovered recordings with interrupted metadata.
Completed call outcomes and published recordings stay available.

Run the process recovery fixture from the workspace root:

```sh
mise x -- cargo test --locked --package rusterisk-server --test process_recovery
```

The fixture starts the server binary, creates completed and active calls, and records known received audio.
It publishes one recording, leaves another recording active, and forcibly terminates and joins the server process.
A second server process uses the same storage directory.
The fixture reads history through a read-only database connection while the restarted server holds the storage lock.
The history query performs no recovery or metadata updates.
The stored-recording API must list both recordings, and both wave files must contain the received signal.
The audio oracle requires at least 80 milliseconds of consecutive matching samples at the file's sample rate.
Rejecting fixtures cover silence, a different signal, and shorter transients at different sample rates.
The fixture also requires an empty channel list and no partial files.
