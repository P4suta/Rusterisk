# Preparing a release

The server archives, Linux images, and Rust client packages share the version in the workspace manifest.
Review the changelog and the workspace version together.
The first proposal targets `0.1.0`.
Later proposals use version tags as their baseline.

## Prepare a version proposal

The Release proposals workflow can open or refresh a draft pull request from `main`.
The `release-proposals` environment provides the Release-plz GitHub App through the Doppler configuration named by `RELEASE_PLZ_DOPPLER_PROJECT` and `RELEASE_PLZ_DOPPLER_CONFIG`.
Store its scoped read-only service token as `DOPPLER_TOKEN` in the environment.
Run `verify-auth` to check read access to `main` before setting `RELEASE_PLZ_PROPOSALS` to `enabled`.

Run the updater in a disposable checkout to inspect a proposal locally.
The first proposal examines up to 100 commits per package.
Use the [compatibility contract](compatibility.md) to review the first changelog's coverage.

```sh
mise x aqua:release-plz/release-plz@0.4.3 -- release-plz update --max-analyze-commits 100
```

Install `cargo-semver-checks` version `0.51.0` for proposals with an existing release tag.
Review the generated version and changelog before moving the draft to review.
When the workspace version changes, update the four Rusterisk dependency requirements in `xtask/fuzz/Cargo.toml` to match.
Update the separate fuzz workspace lockfile and run the fast checks.

```sh
mise x -- cargo update --workspace --manifest-path xtask/fuzz/Cargo.toml
mise x -- just check
mise x -- just security
```

## Verify a candidate

Freeze the source commit before building the [server archives](server-distribution.md) and the [Rust client packages](rust-sdk.md).
Use the required inventory to select the checks.

```sh
mise x -- just release-check --dry-run
```

Use a release input file to name the source checkout, pinned images, candidate paths, and verification outputs.
The [input fixture](../xtask/tests/release_check_cli/inputs.toml) shows the required fields.
Replace the example paths and image identifiers with the exact candidate inputs.
Run each check on its supported host and keep the results with the candidate.

```sh
mise x -- just release-check run fast --inputs "$inputs"
mise x -- just release-check verify --inputs "$inputs"
```

## Assemble the release

Use a signed version tag on the verified commit after approval.
Attach the approved artifacts and checksums to a draft GitHub release.
Confirm the draft's source and asset hashes before publishing the complete release.
Registry publication and platform signing use their own approvals.
