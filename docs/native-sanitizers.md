# Native dependency sanitizer checks

The native check rebuilds bundled Opus, SQLite, and AWS-LC with Clang 14 address and undefined-behavior instrumentation.
Compilation and final linking follow the [Clang AddressSanitizer](https://releases.llvm.org/14.0.0/tools/clang/docs/AddressSanitizer.html#usage) and [UndefinedBehaviorSanitizer](https://releases.llvm.org/14.0.0/tools/clang/docs/UndefinedBehaviorSanitizer.html#usage) instructions.
The scope covers instrumented C exercised by codec, storage, and verified TLS tests.

Build the public verification image and inspect its immutable identity.

```sh
mise x -- just verification-build --dry-run
mise x -- just verification-build --run
native_image="$(mise x -- docker image inspect rusterisk-input-verification:rust-1.99.0 --format '{{.Id}}')"
mise x -- just native-sanitizers --dry-run --image "$native_image"
mise x -- just native-sanitizers --run --image "$native_image"
```

The runner captures project inputs before execution and records their digest under `.cache/native-sanitizers`.
Execution uses a separate owned copy and target directory.
Each container has one processor, 2 GiB of memory without swap, a 256-process limit, and a deadline.
Only dependency fetching has network access.
The root filesystem and source mounts have no write access.
The runner preserves result logs and source bytes while removing execution state and owned containers.

Deliberate C probes must report a freed-memory access and signed integer overflow with the expected exit codes.
The linked native archives must reference both sanitizers.
The runner discovers tests before execution and requires every discovered test to pass with complete summaries.
Skipped tests, missing tests, diagnostics, unexpected probe exits, oversize results, and timeouts fail the check.
The [native instrumentation decision](adr/instrument-native-dependencies-before-boundary-tests.md) describes the boundary.
Run native checks at integration and release stages.
