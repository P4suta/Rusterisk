use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};

use super::{check_sdk_manifest, sdk::MAX_INPUT, SdkManifestError, SdkPackage};
use crate::regular_archive::RegularArchive;

#[derive(Debug)]
pub enum SdkContentError {
    Source,
    Missing(String),
    Changed(String),
    Unexpected(String),
    Encoding,
    Manifest(SdkManifestError),
    LockSyntax(toml::de::Error),
    Lock,
    Targets,
}

impl std::fmt::Display for SdkContentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source => formatter.write_str("SDK content requires bounded, unique captured inputs"),
            Self::Missing(path) => write!(formatter, "SDK archive is missing {path}"),
            Self::Changed(path) => write!(formatter, "SDK archive changed captured bytes for {path}"),
            Self::Unexpected(path) => write!(formatter, "SDK archive contains unexpected {path}"),
            Self::Encoding => formatter.write_str("SDK manifests and lockfiles require bounded UTF-8 text"),
            Self::Manifest(error) => error.fmt(formatter),
            Self::LockSyntax(error) => write!(formatter, "SDK package lockfile: {error}"),
            Self::Lock => formatter.write_str("SDK lockfile requires the exact package and crates.io dependencies without backend crates"),
            Self::Targets => formatter.write_str("SDK public test targets must match the captured top-level tests"),
        }
    }
}

impl std::error::Error for SdkContentError {}

fn captured<'a>(
    inputs: impl IntoIterator<Item = (&'a Path, &'a [u8])>,
) -> Result<BTreeMap<&'a Path, &'a [u8]>, SdkContentError> {
    let mut files = BTreeMap::new();
    let mut total = 0usize;
    for (path, bytes) in inputs {
        total = total
            .checked_add(bytes.len())
            .ok_or(SdkContentError::Source)?;
        if files.len() >= crate::browser::SourceSnapshot::MAX_FILES
            || total > 40 * 1024 * 1024
            || bytes.len() > 8 * 1024 * 1024
            || path.as_os_str().is_empty()
            || !path.components().all(|component| match component {
                Component::Normal(value) => value.to_str().is_some_and(|value| {
                    !value
                        .chars()
                        .any(|character| character.is_control() || character == '\\')
                }),
                Component::Prefix(_)
                | Component::RootDir
                | Component::CurDir
                | Component::ParentDir => false,
            })
            || files.insert(path, bytes).is_some()
        {
            return Err(SdkContentError::Source);
        }
    }
    Ok(files)
}

fn source<'a>(files: &BTreeMap<&Path, &'a [u8]>, path: &Path) -> Result<&'a [u8], SdkContentError> {
    files.get(path).copied().ok_or(SdkContentError::Source)
}

fn entry<'a>(files: &BTreeMap<&str, &'a [u8]>, path: &str) -> Result<&'a [u8], SdkContentError> {
    files
        .get(path)
        .copied()
        .ok_or_else(|| SdkContentError::Missing(path.to_owned()))
}

fn text(bytes: &[u8], limit: usize) -> Result<&str, SdkContentError> {
    if bytes.is_empty() || bytes.len() > limit {
        return Err(SdkContentError::Encoding);
    }
    std::str::from_utf8(bytes).map_err(|_| SdkContentError::Encoding)
}

fn expected<'a>(
    package: SdkPackage,
    captured: &BTreeMap<&Path, &'a [u8]>,
) -> Result<BTreeMap<String, &'a [u8]>, SdkContentError> {
    let root = Path::new(package.directory());
    let mut expected = BTreeMap::new();
    expected.insert(
        "Cargo.toml.orig".to_owned(),
        source(captured, &root.join("Cargo.toml"))?,
    );
    expected.insert(
        "rust-sdk.md".to_owned(),
        source(captured, Path::new("docs/rust-sdk.md"))?,
    );
    for license in ["LICENSE-MIT", "LICENSE-APACHE"] {
        let canonical = source(captured, Path::new(license))?;
        if source(captured, &root.join(license))? != canonical {
            return Err(SdkContentError::Source);
        }
        expected.insert(license.to_owned(), canonical);
    }
    for (path, bytes) in captured {
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        if relative.starts_with("src")
            || (package != SdkPackage::Client && relative.starts_with("tests"))
        {
            expected.insert(relative.to_string_lossy().replace('\\', "/"), *bytes);
        }
    }
    if !expected.contains_key("src/lib.rs") {
        return Err(SdkContentError::Source);
    }
    Ok(expected)
}

fn targets(
    expected: &BTreeMap<String, &[u8]>,
    published: &toml::Value,
) -> Result<(), SdkContentError> {
    let names: BTreeSet<_> = expected
        .keys()
        .filter_map(|path| {
            path.strip_prefix("tests/")
                .filter(|path| !path.contains('/'))
                .filter(|path| path.ends_with(".rs"))
        })
        .map(|path| format!("tests/{path}"))
        .collect();
    let mut declared = BTreeSet::new();
    if let Some(tests) = published.get("test").and_then(toml::Value::as_array) {
        for test in tests {
            let path = test
                .get("path")
                .and_then(toml::Value::as_str)
                .ok_or(SdkContentError::Targets)?;
            declared.insert(path.to_owned());
        }
    }
    if names == declared {
        Ok(())
    } else {
        Err(SdkContentError::Targets)
    }
}

fn lockfile(package: SdkPackage, version: &str, bytes: &[u8]) -> Result<(), SdkContentError> {
    let lock: toml::Value =
        toml::from_str(text(bytes, 2 * 1024 * 1024)?).map_err(SdkContentError::LockSyntax)?;
    let root = lock.as_table().ok_or(SdkContentError::Lock)?;
    if root
        .keys()
        .any(|key| !["version", "package"].contains(&key.as_str()))
        || lock.get("version").and_then(toml::Value::as_integer) != Some(4)
    {
        return Err(SdkContentError::Lock);
    }
    let packages = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or(SdkContentError::Lock)?;
    if packages.is_empty()
        || packages.len() > 1024
        || (package == SdkPackage::Core && packages.len() != 1)
    {
        return Err(SdkContentError::Lock);
    }
    let mut names = BTreeSet::new();
    let mut found = false;
    for item in packages {
        let entry = item.as_table().ok_or(SdkContentError::Lock)?;
        if entry.keys().any(|key| {
            !["name", "version", "source", "checksum", "dependencies"].contains(&key.as_str())
        }) {
            return Err(SdkContentError::Lock);
        }
        let name = item
            .get("name")
            .and_then(toml::Value::as_str)
            .ok_or(SdkContentError::Lock)?;
        let item_version = item
            .get("version")
            .and_then(toml::Value::as_str)
            .ok_or(SdkContentError::Lock)?;
        semver::Version::parse(item_version).map_err(|_| SdkContentError::Lock)?;
        if (name.starts_with("rusterisk-")
            && !SdkPackage::ALL.iter().any(|package| package.name() == name))
            || !names.insert((name, item_version))
        {
            return Err(SdkContentError::Lock);
        }
        if let Some(dependencies) = item.get("dependencies") {
            let dependencies = dependencies.as_array().ok_or(SdkContentError::Lock)?;
            if dependencies
                .iter()
                .any(|dependency| dependency.as_str().is_none_or(str::is_empty))
            {
                return Err(SdkContentError::Lock);
            }
        }
        if name == package.name() {
            if item_version != version
                || item.get("source").is_some()
                || item.get("checksum").is_some()
            {
                return Err(SdkContentError::Lock);
            }
            found = true;
        } else {
            if item.get("source").and_then(toml::Value::as_str)
                != Some("registry+https://github.com/rust-lang/crates.io-index")
            {
                return Err(SdkContentError::Lock);
            }
            let checksum = item
                .get("checksum")
                .and_then(toml::Value::as_str)
                .ok_or(SdkContentError::Lock)?;
            if checksum.len() != 64
                || !checksum
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(SdkContentError::Lock);
            }
        }
    }
    if found {
        Ok(())
    } else {
        Err(SdkContentError::Lock)
    }
}

pub fn check_sdk_content<'a>(
    package: SdkPackage,
    source_files: impl IntoIterator<Item = (&'a Path, &'a [u8])>,
    archive: &RegularArchive,
) -> Result<(), SdkContentError> {
    let captured = captured(source_files)?;
    let expected = expected(package, &captured)?;
    let actual: BTreeMap<_, _> = archive
        .files()
        .iter()
        .map(|file| (file.path().as_str(), file.bytes()))
        .collect();
    for (path, bytes) in &expected {
        if entry(&actual, path)? != *bytes {
            return Err(SdkContentError::Changed(path.clone()));
        }
    }
    for path in actual.keys() {
        if !expected.contains_key(*path) && !["Cargo.toml", "Cargo.lock"].contains(path) {
            return Err(SdkContentError::Unexpected((*path).to_owned()));
        }
    }
    let manifest = text(entry(&actual, "Cargo.toml")?, MAX_INPUT)?;
    let original = text(entry(&actual, "Cargo.toml.orig")?, MAX_INPUT)?;
    let workspace = text(source(&captured, Path::new("Cargo.toml"))?, MAX_INPUT)?;
    check_sdk_manifest(package, original, workspace, manifest)
        .map_err(SdkContentError::Manifest)?;
    let published: toml::Value = toml::from_str(manifest)
        .map_err(SdkManifestError::Syntax)
        .map_err(SdkContentError::Manifest)?;
    targets(&expected, &published)?;
    let version = published
        .get("package")
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str)
        .ok_or(SdkContentError::Lock)?;
    lockfile(package, version, entry(&actual, "Cargo.lock")?)
}
