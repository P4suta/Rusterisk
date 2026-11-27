# Preparing a release

The server archives, Linux images, and remote CLI share the version in the workspace manifest.
Only `rusterisk-cli` permits publication to crates.io.
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

Freeze the source commit before building the [server archives](server-distribution.md) and the [remote CLI](../crates/cli/README.md).
Verify the CLI archive by running its packaged tests, installing the command in a temporary directory, and checking its version.

```sh
mise x -- just distribution-cli-check
```

Rehearse the crates.io upload from the selected commit with Cargo.
The dry run builds and verifies the standalone CLI archive.

```sh
mise x -- cargo publish --dry-run --locked --registry crates-io -p rusterisk-cli
```

The server integration checks use three private [Rust SDK libraries](rust-sdk.md).
Their offline Cargo packaging workspace enables a fixture registry so that the client lockfile records the exact core and API archive checksums.
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

## Publish the verified downloads

After verification and any required platform signing, place a signed, annotated version tag on the exact verified commit.
Set `version` to the version without the leading `v`, `source` to the full commit ID, and `assets` to the verified distribution directory.
The directory contains five server archives, two Linux image archives, the CLI crate, twelve CycloneDX files under `sbom/`, and `SHA256SUMS`.
The local check reads the recorded digests and the source commit embedded in the CLI archive.

```sh
mise x -- just release-publication plan "$version" "$source" "$assets"
```

Run the submission command when the release is ready for approval.

```sh
mise x -- just release-publication submit "$version" "$source" "$assets"
```

Submission creates a draft with generated release notes, attaches the verified files, and starts `publish-cli.yml` on `main`.
GitHub stores attachments in a single directory, so the uploaded checksum file uses the attachment names.
Use the digest printed by the staging command for a manual workflow run.
The `stage` operation stops after verifying the draft.
A repeated submission reuses matching attachments and rejects conflicting contents.
Staging uses private copies of the verified files and retries empty uploads left on the draft.

The workflow builds its controller from the protected default branch.
The candidate job needs GitHub push access to read draft releases and exports the verified CLI bytes.
The `crates-io` environment controls publication of the original archive through [crates.io Trusted Publishing](https://crates.io/docs/trusted-publishing).
Register `P4suta/Rusterisk`, workflow filename `publish-cli.yml`, and environment `crates-io` in the settings for `rusterisk-cli`.
The `verify-auth` operation exchanges the GitHub identity for a temporary credential and revokes it at the end of the job.

```sh
gh workflow run publish-cli.yml --repo P4suta/Rusterisk --ref main -f operation=verify-auth
```

The `publish-cli` operation publishes only to crates.io.
The `publish-release` operation also requests approval from the `github-release` environment.
Configure the environment's `DOPPLER_TOKEN` for read access to the release App config named by `RELEASE_APP_DOPPLER_PROJECT` and `RELEASE_APP_DOPPLER_CONFIG`.
The App needs the [Administration read permission](https://docs.github.com/en/rest/repos/repos#check-if-immutable-releases-are-enabled-for-a-repository) on `P4suta/Rusterisk` to read the repository setting.
The final job checks the signed tag, all asset digests, and the crates.io receipt before making the [immutable release](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases) public.
Keep the draft and its repository settings unchanged during publication.
GitHub handles the final inventory read, settings read, and publication as separate requests.
An interrupted run can resume with the same version, source, and checksum file.
