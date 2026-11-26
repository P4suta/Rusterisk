---
date: 2026-11-27
status: proposed
---

# Publish verified assets from a signed tag

## Context

Five server archives, two Linux images, the standalone CLI, and their inventories are verified on different hosts.
The CLI archive embeds its source commit.

## Decision

Stage the original files on a GitHub draft after checking the signed tag and each checksum.
Bind the protected publication workflow to the source commit and the digest of the checksum file.
Use a separate environment for each public destination.
Send the approved archive through the [crates.io publish endpoint](https://doc.rust-lang.org/cargo/reference/registry-web-api.html#publish) with [Trusted Publishing](https://crates.io/docs/trusted-publishing).
Publish the [immutable GitHub release](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases) after the registry and draft agree with the approved bytes.

## Alternatives

Rebuilding from a tag in the publication job would require repeating the native verification for the rebuilt files.

## Consequences

Retrying a partial upload requires the original bytes.
Reconsider the staging source when a hosted build can retain every verified native artifact.

## Confirmation

Exercise missing, matching, and conflicting assets on a partial draft.
Check that authentication alone sends no upload and that a completed release requires no further writes.
