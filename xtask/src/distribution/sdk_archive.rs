use super::sdk::{manifest, read, SdkInputError, MAX_INPUT};
use std::path::Path;

const CLIENT_MANIFEST: &str = "crates/client/Cargo.toml";

#[derive(Debug)]
pub enum SdkArchiveError {
    Input(SdkInputError),
    WorkspaceVersion,
    Dependency { name: &'static str },
    Exclusions,
}

impl std::fmt::Display for SdkArchiveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(error) => error.fmt(formatter),
            Self::WorkspaceVersion => {
                formatter.write_str("Cargo.toml: require workspace.package.version")
            }
            Self::Dependency { name } => write!(
                formatter,
                "{CLIENT_MANIFEST}: invalid SDK dependency {name}"
            ),
            Self::Exclusions => write!(
                formatter,
                "{CLIENT_MANIFEST}: exclude tests/** and examples/** without an overriding include"
            ),
        }
    }
}

impl std::error::Error for SdkArchiveError {}

impl From<SdkInputError> for SdkArchiveError {
    fn from(error: SdkInputError) -> Self {
        Self::Input(error)
    }
}

fn parse(path: &str, bytes: &[u8]) -> Result<toml::Value, SdkArchiveError> {
    if bytes.is_empty() || bytes.len() > MAX_INPUT {
        return Err(SdkInputError::Size {
            path: path.to_owned(),
        }
        .into());
    }
    Ok(manifest(path, bytes)?)
}

pub fn validate_sdk_archive_inputs(client: &str, version: &str) -> Result<(), SdkArchiveError> {
    if version.is_empty() {
        return Err(SdkArchiveError::WorkspaceVersion);
    }
    let source = parse(CLIENT_MANIFEST, client.as_bytes())?;
    for (name, path) in [("rusterisk-api", "../api"), ("rusterisk-core", "../core")] {
        let dependency = source.get("dependencies").and_then(|table| table.get(name));
        if dependency
            .and_then(|value| value.get("path"))
            .and_then(toml::Value::as_str)
            != Some(path)
            || dependency
                .and_then(|value| value.get("version"))
                .and_then(toml::Value::as_str)
                != Some(version)
        {
            return Err(SdkArchiveError::Dependency { name });
        }
    }
    for (name, path) in [
        ("rusterisk-server", "../server"),
        ("rusterisk-config", "../config"),
        ("rusterisk-media", "../media"),
    ] {
        let dependency = source
            .get("dev-dependencies")
            .and_then(|table| table.get(name))
            .and_then(toml::Value::as_table);
        if dependency.is_none_or(|value| {
            value.len() != 1 || value.get("path").and_then(toml::Value::as_str) != Some(path)
        }) {
            return Err(SdkArchiveError::Dependency { name });
        }
    }
    let metadata = source.get("package");
    let exclusions = metadata
        .and_then(|value| value.get("exclude"))
        .and_then(toml::Value::as_array);
    if metadata.and_then(|value| value.get("include")).is_some()
        || exclusions.is_none_or(|values| {
            values.len() != 2
                || ["tests/**", "examples/**"].iter().any(|expected| {
                    values
                        .iter()
                        .filter(|value| value.as_str() == Some(expected))
                        .count()
                        != 1
                })
        })
    {
        return Err(SdkArchiveError::Exclusions);
    }
    Ok(())
}

pub fn check_sdk_archive_inputs(root: &Path) -> Result<(), SdkArchiveError> {
    let workspace = parse("Cargo.toml", &read(root, "Cargo.toml")?)?;
    let version = workspace
        .get("workspace")
        .and_then(|value| value.get("package"))
        .and_then(|value| value.get("version"))
        .and_then(toml::Value::as_str)
        .ok_or(SdkArchiveError::WorkspaceVersion)?;
    let bytes = read(root, CLIENT_MANIFEST)?;
    let client = std::str::from_utf8(&bytes).map_err(|_| SdkInputError::Encoding {
        path: CLIENT_MANIFEST.to_owned(),
    })?;
    validate_sdk_archive_inputs(client, version)
}
