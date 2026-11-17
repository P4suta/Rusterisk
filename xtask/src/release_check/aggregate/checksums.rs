#![cfg(unix)]

use super::{
    require_same_sdk, require_same_server, ArchiveSha256, SdkArchiveIdentity, ServerArchiveIdentity,
};
use crate::{
    distribution::{SdkContents, SdkContentsError, SdkPackage},
    release_check::{
        candidate_fixtures::{linux_target, require, using, Candidates},
        RequiredGate,
    },
    server_distribution::ServerCandidate,
};
use sha2::{Digest, Sha256};
use std::{fs, io, io::Read, path::Path};

const SDK_LIMIT: usize = 16 * 1024 * 1024;
const SERVER_LIMIT: usize = 272 * 1024 * 1024;

fn read(path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    require(
        metadata.is_file() && metadata.len() <= maximum as u64,
        "Checksum fixture requires a bounded regular archive",
    )?;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    require(
        bytes.len() == metadata.len() as usize && bytes.len() <= maximum,
        "Checksum fixture archive changed while reading",
    )?;
    Ok(bytes)
}

fn checksum(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn change_gzip_timestamp(path: &Path, maximum: usize) -> io::Result<()> {
    let mut bytes = read(path, maximum)?;
    require(
        bytes.starts_with(&[0x1f, 0x8b, 8]),
        "Metadata fixture requires a gzip archive",
    )?;
    require(
        bytes.get(3).is_some_and(|flags| flags & 2 == 0),
        "Metadata fixture requires a gzip header without a header checksum",
    )?;
    let timestamp = bytes
        .get_mut(4)
        .ok_or_else(|| io::Error::other("Gzip timestamp is missing"))?;
    *timestamp ^= 1;
    fs::write(path, bytes)
}

fn update_sdk_checksums(directory: &Path, version: &str) -> io::Result<()> {
    let mut sidecar = String::new();
    for package in SdkPackage::ALL {
        let name = format!("{}-{version}.crate", package.name());
        let digest = checksum(&read(&directory.join(&name), SDK_LIMIT)?);
        sidecar.push_str(&format!("{digest}  {name}\n"));
    }
    fs::write(directory.join("SHA256SUMS"), sidecar)
}

fn sdk_digest_mismatch(
    paths: &Candidates,
    gate: RequiredGate,
    package: SdkPackage,
) -> io::Result<()> {
    let (source, contents) = SdkContents::read_paired(&paths.sdk).map_err(io::Error::other)?;
    let expected =
        SdkArchiveIdentity::from_candidate(&source, &contents).map_err(io::Error::other)?;
    require_same_sdk(gate, &expected, &expected).map_err(io::Error::other)?;
    let other = match package {
        SdkPackage::Core => SdkPackage::Api,
        SdkPackage::Api => SdkPackage::Client,
        SdkPackage::Client => SdkPackage::Core,
    };
    require(
        contents.checksum(package) != contents.checksum(other),
        "SDK fixture archives have equal digests",
    )?;
    let replacement =
        ArchiveSha256::parse(contents.checksum(other), gate).map_err(io::Error::other)?;
    let mut changed =
        SdkArchiveIdentity::from_candidate(&source, &contents).map_err(io::Error::other)?;
    let [core, api, client] = &mut changed.checksums;
    match package {
        SdkPackage::Core => *core = replacement,
        SdkPackage::Api => *api = replacement,
        SdkPackage::Client => *client = replacement,
    }
    require(
        expected.version == changed.version && expected.projection == changed.projection,
        "Pure SDK counterexample changed another identity role",
    )?;
    for ((selected, original), counterexample) in SdkPackage::ALL
        .into_iter()
        .zip(expected.checksums)
        .zip(changed.checksums)
    {
        require(
            (original != counterexample) == (selected == package),
            "Pure SDK counterexample changed another package digest",
        )?;
    }
    require(
        require_same_sdk(gate, &expected, &changed).is_err(),
        "A nested consumer substituted another package archive digest",
    )
}

#[test]
fn server_sdk_bindings_reject_another_archive_digest_for_each_package() -> io::Result<()> {
    let target = linux_target()?.server_target();
    for package in SdkPackage::ALL {
        using(target, |_captured, paths| {
            sdk_digest_mismatch(paths, RequiredGate::Server(target), package)
        })?;
    }
    Ok(())
}

#[test]
fn oci_nested_sdk_bindings_reject_another_archive_digest_for_each_package() -> io::Result<()> {
    let target = linux_target()?;
    for package in SdkPackage::ALL {
        using(target.server_target(), |_captured, paths| {
            sdk_digest_mismatch(paths, RequiredGate::Oci(target), package)
        })?;
    }
    Ok(())
}

#[test]
fn oci_server_binding_rejects_another_actual_archive_digest() -> io::Result<()> {
    let target = linux_target()?;
    using(target.server_target(), |_captured, paths| {
        let (source, candidate) = ServerCandidate::read_paired(&paths.server, paths.target)?;
        let expected =
            ServerArchiveIdentity::from_candidate(&source, &candidate).map_err(io::Error::other)?;
        require_same_server(target, &expected, &expected).map_err(io::Error::other)?;
        candidate.check_oci(paths.archive()?)?;
        let (_sdk_source, sdk) = SdkContents::read_paired(&paths.sdk).map_err(io::Error::other)?;
        let other = sdk.checksum(SdkPackage::Client);
        require(
            candidate.checksum() != other,
            "Server and SDK fixture archives have equal digests",
        )?;
        let mut changed =
            ServerArchiveIdentity::from_candidate(&source, &candidate).map_err(io::Error::other)?;
        changed.checksum =
            ArchiveSha256::parse(other, RequiredGate::Oci(target)).map_err(io::Error::other)?;
        require(
            expected.target == changed.target
                && expected.version == changed.version
                && expected.projection == changed.projection
                && expected.source_digest == changed.source_digest,
            "Pure server counterexample changed another identity role",
        )?;
        require(
            require_same_server(target, &expected, &changed).is_err(),
            "An OCI server binding substituted another archive digest",
        )
    })
}

#[test]
fn sdk_dependency_checksums_reject_a_repacked_core_archive() -> io::Result<()> {
    let target = linux_target()?.server_target();
    using(target, |_captured, paths| {
        let (_source, contents) = SdkContents::read_paired(&paths.sdk).map_err(io::Error::other)?;
        let name = format!("{}-{}.crate", SdkPackage::Core.name(), contents.version());
        change_gzip_timestamp(&paths.sdk.join(name), SDK_LIMIT)?;
        update_sdk_checksums(&paths.sdk, contents.version())?;
        let error = SdkContents::read_paired(&paths.sdk).err().ok_or_else(|| {
            io::Error::other("The SDK reader accepted changed dependency archive bytes")
        })?;
        if matches!(
            &error,
            SdkContentsError::Contract("same-candidate dependency checksum")
        ) {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "Unexpected SDK reader rejection: {error}"
            )))
        }
    })
}

#[test]
fn server_reader_rejects_gzip_metadata_after_checksum_sidecar_update() -> io::Result<()> {
    let target = linux_target()?.server_target();
    using(target, |_captured, paths| {
        let (_source, candidate) = ServerCandidate::read_paired(&paths.server, paths.target)?;
        let name = candidate.name().file_name();
        let archive = paths.server.join(name);
        change_gzip_timestamp(&archive, SERVER_LIMIT)?;
        let digest = checksum(&read(&archive, SERVER_LIMIT)?);
        fs::write(
            paths.server.join("SHA256SUMS"),
            format!("{digest}  {name}\n"),
        )?;
        let error = ServerCandidate::read_paired(&paths.server, paths.target)
            .err()
            .ok_or_else(|| {
                io::Error::other("The server reader accepted noncanonical gzip metadata")
            })?;
        require(
            error.kind() == io::ErrorKind::Other
                && error.to_string() == "server archive has unexpected gzip metadata",
            "Server reader lost its canonical gzip metadata rejection",
        )
    })
}
