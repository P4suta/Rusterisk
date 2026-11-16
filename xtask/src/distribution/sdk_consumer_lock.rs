use super::{SdkConsumerScope, SdkPackage, SdkRuntimeIdentity};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub enum SdkConsumerLockError {
    Size,
    Encoding,
    Syntax(toml::de::Error),
    Contract(&'static str),
    Registry { name: String, version: String },
}

impl std::fmt::Display for SdkConsumerLockError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Size => formatter
                .write_str("SDK consumer lockfiles require bounded text and package counts"),
            Self::Encoding => formatter.write_str("SDK consumer lockfiles require UTF-8 text"),
            Self::Syntax(error) => write!(formatter, "SDK consumer lockfile: {error}"),
            Self::Contract(field) => write!(formatter, "SDK consumer lockfile: invalid {field}"),
            Self::Registry { name, version } => write!(
                formatter,
                "SDK consumer lockfile: registry identity changed for {name}@{version}"
            ),
        }
    }
}
impl std::error::Error for SdkConsumerLockError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lock {
    version: u8,
    package: Vec<Package>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
}

fn invalid(field: &'static str) -> SdkConsumerLockError {
    SdkConsumerLockError::Contract(field)
}
fn parse(bytes: &[u8]) -> Result<Lock, SdkConsumerLockError> {
    if bytes.is_empty() || bytes.len() > 2 * 1024 * 1024 {
        return Err(SdkConsumerLockError::Size);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| SdkConsumerLockError::Encoding)?;
    let lock: Lock = toml::from_str(text).map_err(SdkConsumerLockError::Syntax)?;
    if lock.version != 4 {
        return Err(invalid("lockfile version"));
    }
    if lock.package.is_empty() || lock.package.len() > 1024 {
        return Err(SdkConsumerLockError::Size);
    }
    for package in &lock.package {
        if package.name.is_empty()
            || package.name.len() > 128
            || !package
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || semver::Version::parse(&package.version).is_err()
            || package.dependencies.len() > 1024
            || package.dependencies.iter().any(|dependency| {
                dependency.is_empty()
                    || dependency.len() > 4096
                    || dependency.chars().any(char::is_control)
            })
        {
            return Err(invalid("package fields"));
        }
    }
    Ok(lock)
}

fn sdk(name: &str) -> Option<SdkPackage> {
    SdkPackage::ALL
        .into_iter()
        .find(|package| package.name() == name)
}
fn selected(scope: SdkConsumerScope) -> &'static [SdkPackage] {
    match scope {
        SdkConsumerScope::CoreMsrv => &[SdkPackage::Core],
        SdkConsumerScope::Sdk => &SdkPackage::ALL,
    }
}
fn plain(package: &Package, version: &str) -> Result<(), SdkConsumerLockError> {
    if package.version != version || package.source.is_some() || package.checksum.is_some() {
        Err(invalid("local package identity"))
    } else {
        Ok(())
    }
}

fn registry(package: &Package) -> Result<(&str, &str), SdkConsumerLockError> {
    let source = package
        .source
        .as_deref()
        .ok_or_else(|| invalid("registry source"))?;
    let checksum = package
        .checksum
        .as_deref()
        .ok_or_else(|| invalid("registry checksum"))?;
    if !source.split_once('+').is_some_and(|(kind, opaque)| {
        ["registry", "sparse"].contains(&kind)
            && !opaque.is_empty()
            && opaque.len() <= 16384
            && !opaque.chars().any(char::is_control)
    }) || checksum.len() != 64
        || !checksum
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("registry identity"));
    }
    Ok((source, checksum))
}

pub(super) fn runtime(bytes: &[u8]) -> Result<SdkRuntimeIdentity, SdkConsumerLockError> {
    let lock = parse(bytes)?;
    let mut packages = lock
        .package
        .iter()
        .filter(|package| package.name == "tokio");
    let package = packages
        .next()
        .ok_or_else(|| invalid("candidate Tokio runtime"))?;
    if packages.next().is_some() {
        return Err(invalid("single candidate Tokio runtime"));
    }
    let (source, checksum) = registry(package)?;
    Ok(SdkRuntimeIdentity {
        version: package.version.clone(),
        source: source.to_owned(),
        checksum: checksum.to_owned(),
    })
}

type RegistryIdentities<'a> = BTreeMap<(&'a str, &'a str, &'a str), &'a str>;

fn identities<'a>(
    scope: SdkConsumerScope,
    version: &str,
    seed: &'a Lock,
) -> Result<RegistryIdentities<'a>, SdkConsumerLockError> {
    let mut identities = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for package in &seed.package {
        if let Some(sdk) = sdk(&package.name) {
            if package.version != version
                || !selected(scope).contains(&sdk)
                || !seen.insert(sdk.name())
            {
                return Err(invalid("seed SDK package identity"));
            }
            match (scope, sdk) {
                (SdkConsumerScope::CoreMsrv, SdkPackage::Core)
                | (SdkConsumerScope::Sdk, SdkPackage::Client) => plain(package, version)?,
                (SdkConsumerScope::Sdk, SdkPackage::Core | SdkPackage::Api) => {
                    registry(package)?;
                }
                (SdkConsumerScope::CoreMsrv, SdkPackage::Api | SdkPackage::Client) => {
                    return Err(invalid("seed core scope"))
                }
            }
        } else {
            if package.name.starts_with("rusterisk-") {
                return Err(invalid("seed backend package"));
            }
            let (source, checksum) = registry(package)?;
            if identities
                .insert(
                    (package.name.as_str(), package.version.as_str(), source),
                    checksum,
                )
                .is_some()
            {
                return Err(invalid("duplicate seed registry package"));
            }
        }
    }
    if seen.len() != selected(scope).len()
        || (scope == SdkConsumerScope::CoreMsrv && seed.package.len() != 1)
    {
        return Err(invalid("seed SDK package set"));
    }
    Ok(identities)
}

pub fn check_sdk_consumer_lock(
    scope: SdkConsumerScope,
    version: &str,
    seed: &[u8],
    resolved: &[u8],
) -> Result<(), SdkConsumerLockError> {
    semver::Version::parse(version).map_err(|_| invalid("candidate version"))?;
    let seed = parse(seed)?;
    let resolved = parse(resolved)?;
    let allowed = identities(scope, version, &seed)?;
    let mut packages = BTreeSet::new();
    let mut project = BTreeSet::new();
    for package in &resolved.package {
        if !packages.insert((
            package.name.as_str(),
            package.version.as_str(),
            package.source.as_deref(),
        )) {
            return Err(invalid("duplicate resolved package"));
        }
        if package.name == scope.package_name() {
            plain(package, "0.0.0")?;
            project.insert(package.name.as_str());
        } else if let Some(sdk) = sdk(&package.name) {
            if !selected(scope).contains(&sdk) {
                return Err(invalid("unselected SDK package"));
            }
            plain(package, version)?;
            project.insert(package.name.as_str());
        } else {
            if package.name.starts_with("rusterisk-") {
                return Err(invalid("backend package"));
            }
            let (source, checksum) = registry(package)?;
            if allowed
                .get(&(package.name.as_str(), package.version.as_str(), source))
                .copied()
                != Some(checksum)
            {
                return Err(SdkConsumerLockError::Registry {
                    name: package.name.clone(),
                    version: package.version.clone(),
                });
            }
        }
    }
    if project.len() != selected(scope).len() + 1 {
        return Err(invalid("consumer and SDK package set"));
    }
    Ok(())
}
