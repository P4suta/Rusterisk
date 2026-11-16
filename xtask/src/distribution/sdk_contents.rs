use std::{
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
};

use super::{check_sdk_content, CapturedSdk, SdkCandidateBuildError, SdkContentError, SdkPackage};
use crate::regular_archive::{
    read_regular_gzip, ArchiveLimits, ArchiveRoot, RegularArchive, RegularArchiveError,
};
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub enum SdkContentsError {
    Io(std::io::Error),
    Source(SdkCandidateBuildError),
    Archive(RegularArchiveError),
    Content(SdkContentError),
    Contract(&'static str),
}

impl std::fmt::Display for SdkContentsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Source(error) => error.fmt(formatter),
            Self::Archive(error) => error.fmt(formatter),
            Self::Content(error) => error.fmt(formatter),
            Self::Contract(field) => write!(formatter, "SDK candidate content: invalid {field}"),
        }
    }
}

impl std::error::Error for SdkContentsError {}

impl From<std::io::Error> for SdkContentsError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

struct PackageContents {
    archive: RegularArchive,
    checksum: String,
}

pub struct SdkContents {
    version: String,
    source_digest: String,
    core: PackageContents,
    api: PackageContents,
    client: PackageContents,
}

fn directory(path: &Path) -> Result<PathBuf, SdkContentsError> {
    let path = std::path::absolute(path)?;
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(SdkContentsError::Contract("candidate directory"));
    }
    for ancestor in path.ancestors() {
        if !std::fs::symlink_metadata(ancestor)?.file_type().is_dir() {
            return Err(SdkContentsError::Contract("canonical candidate directory"));
        }
    }
    Ok(path)
}

fn read(path: &Path, limit: u64) -> Result<Vec<u8>, SdkContentsError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > limit {
        return Err(SdkContentsError::Contract("bounded regular candidate file"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > limit {
        return Err(SdkContentsError::Contract("bounded regular candidate file"));
    }
    Ok(bytes)
}

fn name(package: SdkPackage, version: &str) -> String {
    format!("{}-{version}.crate", package.name())
}

fn checksum<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    package: SdkPackage,
    version: &str,
) -> Result<String, SdkContentsError> {
    let (digest, file) = lines
        .next()
        .and_then(|line| line.split_once("  "))
        .ok_or(SdkContentsError::Contract("SHA256SUMS"))?;
    if file != name(package, version)
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(SdkContentsError::Contract("SHA256SUMS"));
    }
    Ok(digest.to_owned())
}

fn manifest(archive: &RegularArchive, path: &str) -> Result<toml::Value, SdkContentsError> {
    let bytes = archive
        .files()
        .iter()
        .find(|file| file.path().as_str() == path)
        .ok_or(SdkContentsError::Contract("package metadata"))?
        .bytes();
    let text =
        std::str::from_utf8(bytes).map_err(|_| SdkContentsError::Contract("package metadata"))?;
    toml::from_str(text).map_err(|_| SdkContentsError::Contract("package metadata"))
}

fn package(
    candidate: &Path,
    source: &CapturedSdk,
    package: SdkPackage,
    checksum: String,
) -> Result<PackageContents, SdkContentsError> {
    let bytes = read(
        &candidate.join(name(package, source.version())),
        16 * 1024 * 1024,
    )?;
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if digest != checksum {
        return Err(SdkContentsError::Contract("archive checksum"));
    }
    let root = ArchiveRoot::new(&format!("{}-{}", package.name(), source.version()))
        .map_err(|_| SdkContentsError::Contract("archive root"))?;
    let archive = read_regular_gzip(Cursor::new(bytes), &root, ArchiveLimits::SDK)
        .map_err(SdkContentsError::Archive)?;
    check_sdk_content(package, source.files(), &archive).map_err(SdkContentsError::Content)?;
    let published = manifest(&archive, "Cargo.toml")?;
    if published
        .get("package")
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str)
        != Some(source.version())
    {
        return Err(SdkContentsError::Contract("workspace package version"));
    }
    Ok(PackageContents { archive, checksum })
}

impl SdkContents {
    pub fn read(candidate: &Path) -> Result<Self, SdkContentsError> {
        let candidate = directory(candidate)?;
        let source =
            CapturedSdk::read(&candidate.join("source")).map_err(SdkContentsError::Source)?;
        let source_digest = source.source_digest();
        if read(&candidate.join("SOURCE.sha256"), 65)? != format!("{source_digest}\n").as_bytes() {
            return Err(SdkContentsError::Contract("SOURCE.sha256"));
        }
        for (index, entry) in std::fs::read_dir(&candidate)?.enumerate() {
            if index >= 64 {
                return Err(SdkContentsError::Contract("candidate entry count"));
            }
            let entry = entry?;
            let path = entry.path();
            if path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("crate"))
                && !SdkPackage::ALL
                    .iter()
                    .any(|package| entry.file_name() == name(*package, source.version()).as_str())
            {
                return Err(SdkContentsError::Contract("three-package candidate set"));
            }
        }
        let sums = read(&candidate.join("SHA256SUMS"), 1024)?;
        let sums =
            std::str::from_utf8(&sums).map_err(|_| SdkContentsError::Contract("SHA256SUMS"))?;
        if !sums.ends_with('\n') {
            return Err(SdkContentsError::Contract("SHA256SUMS"));
        }
        let mut lines = sums.split_terminator('\n');
        let core = checksum(&mut lines, SdkPackage::Core, source.version())?;
        let api = checksum(&mut lines, SdkPackage::Api, source.version())?;
        let client = checksum(&mut lines, SdkPackage::Client, source.version())?;
        if lines.next().is_some() {
            return Err(SdkContentsError::Contract("SHA256SUMS"));
        }
        let contents = Self {
            version: source.version().to_owned(),
            source_digest,
            core: package(&candidate, &source, SdkPackage::Core, core)?,
            api: package(&candidate, &source, SdkPackage::Api, api)?,
            client: package(&candidate, &source, SdkPackage::Client, client)?,
        };
        contents.check_locks()?;
        Ok(contents)
    }

    fn package(&self, package: SdkPackage) -> &PackageContents {
        match package {
            SdkPackage::Core => &self.core,
            SdkPackage::Api => &self.api,
            SdkPackage::Client => &self.client,
        }
    }

    pub fn archive(&self, package: SdkPackage) -> &RegularArchive {
        &self.package(package).archive
    }
    pub fn checksum(&self, package: SdkPackage) -> &str {
        &self.package(package).checksum
    }
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
    pub fn version(&self) -> &str {
        &self.version
    }

    fn check_locks(&self) -> Result<(), SdkContentsError> {
        for package in SdkPackage::ALL {
            let lock = manifest(self.archive(package), "Cargo.lock")?;
            let entries = lock
                .get("package")
                .and_then(toml::Value::as_array)
                .ok_or(SdkContentsError::Contract("lockfile packages"))?;
            let mut core = false;
            let mut api = false;
            for entry in entries {
                let name = entry
                    .get("name")
                    .and_then(toml::Value::as_str)
                    .ok_or(SdkContentsError::Contract("lockfile package name"))?;
                if !name.starts_with("rusterisk-") || name == package.name() {
                    continue;
                }
                let peer = match (package, name) {
                    (SdkPackage::Client, "rusterisk-core") => {
                        core = true;
                        SdkPackage::Core
                    }
                    (SdkPackage::Client, "rusterisk-api") => {
                        api = true;
                        SdkPackage::Api
                    }
                    _ => return Err(SdkContentsError::Contract("SDK lockfile dependency")),
                };
                if entry.get("version").and_then(toml::Value::as_str) != Some(self.version())
                    || entry.get("checksum").and_then(toml::Value::as_str)
                        != Some(self.checksum(peer))
                {
                    return Err(SdkContentsError::Contract(
                        "same-candidate dependency checksum",
                    ));
                }
            }
            if package == SdkPackage::Client && !(core && api) {
                return Err(SdkContentsError::Contract(
                    "client lockfile SDK dependencies",
                ));
            }
        }
        Ok(())
    }
}
