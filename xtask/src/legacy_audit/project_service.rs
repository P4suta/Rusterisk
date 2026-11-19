use super::{AuditError, Lock, LockedPackage};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

const DIRECTORY: &str = "project-service-only";
const LINK: &str = "node_modules/tinyglobby";
const NAME: &str = "@rusterisk/project-service-only";
const LICENSE: &str = "(MIT OR Apache-2.0)";
const RESOLVER: &str = "@typescript-eslint/typescript-estree";
const FILES: [&str; 3] = ["package.json", "index.mjs", "index.ts"];
const ENTRY: &str =
    include_str!("../../../tests/interop/node-ari-client/project-service-only/index.mjs");
const SOURCE: &str =
    include_str!("../../../tests/interop/node-ari-client/project-service-only/index.ts");
#[derive(Clone, Copy)]
pub(super) enum FixtureRole {
    Comparison,
    Browser,
}

impl FixtureRole {
    fn entry(self) -> &'static str {
        match self {
            Self::Comparison => DIRECTORY,
            Self::Browser => "../../tests/interop/node-ari-client/project-service-only",
        }
    }

    fn override_path(self) -> &'static str {
        match self {
            Self::Comparison => "file:../../../project-service-only",
            Self::Browser => {
                "file:../../../../../tests/interop/node-ari-client/project-service-only"
            }
        }
    }

    fn directory(self, root: &Path) -> Result<PathBuf, AuditError> {
        if !fs::symlink_metadata(root)?.is_dir() {
            return Err(AuditError::Boundary);
        }
        let (mut directory, components): (PathBuf, &[&str]) = match self {
            Self::Comparison => (root.to_owned(), &[DIRECTORY]),
            Self::Browser => {
                let examples = root.parent().ok_or(AuditError::Boundary)?;
                if !fs::symlink_metadata(examples)?.is_dir() {
                    return Err(AuditError::Boundary);
                }
                (
                    examples.parent().ok_or(AuditError::Boundary)?.to_owned(),
                    &["tests", "interop", "node-ari-client", DIRECTORY],
                )
            }
        };
        for component in components {
            directory.push(component);
            if !fs::symlink_metadata(&directory)?.is_dir() {
                return Err(AuditError::Boundary);
            }
        }
        Ok(directory)
    }

    fn eslint(self) -> &'static str {
        match self {
            Self::Comparison => {
                include_str!("../../../tests/interop/node-ari-client/eslint.config.mjs")
            }
            Self::Browser => include_str!("../../../examples/browser/eslint.config.mjs"),
        }
    }

    fn typescript(self) -> &'static str {
        match self {
            Self::Comparison => {
                include_str!("../../../tests/interop/node-ari-client/tsconfig.json")
            }
            Self::Browser => include_str!("../../../examples/browser/tsconfig.json"),
        }
    }
}

pub(super) struct Verified {
    role: FixtureRole,
}

impl Verified {
    pub(super) fn contains(&self, path: &str) -> bool {
        path == self.role.entry() || path == LINK
    }

    pub(super) fn allows_runtime(&self, path: &str, package: &LockedPackage) -> bool {
        matches!(self.role, FixtureRole::Browser)
            && path == "node_modules/sip.js"
            && package.version.as_deref() == Some("0.21.2")
    }
}

#[derive(Deserialize)]
struct RootManifest {
    overrides: BTreeMap<String, ResolverOverride>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolverOverride {
    minimatch: String,
    tinyglobby: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdapterManifest {
    name: String,
    private: bool,
    #[serde(rename = "type")]
    kind: String,
    license: String,
    exports: String,
}

pub(super) fn read_regular(path: &Path) -> Result<String, AuditError> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(AuditError::Boundary);
    }
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(AuditError::Boundary);
    }
    if metadata.len() > 1024 * 1024 {
        return Err(AuditError::Capacity);
    }
    let mut text = String::new();
    file.take(1024 * 1024 + 1).read_to_string(&mut text)?;
    if text.len() > 1024 * 1024 {
        return Err(AuditError::Capacity);
    }
    Ok(text)
}

fn sources(root: &Path, role: FixtureRole) -> Result<(), AuditError> {
    let directory = role.directory(root)?;
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| AuditError::Boundary)?;
        if !FILES.contains(&name.as_str()) || !entry.file_type()?.is_file() || !names.insert(name) {
            return Err(AuditError::Boundary);
        }
    }
    if names.len() != FILES.len()
        || read_regular(&directory.join("index.mjs"))? != ENTRY
        || read_regular(&directory.join("index.ts"))? != SOURCE
        || read_regular(&root.join("eslint.config.mjs"))? != role.eslint()
        || read_regular(&root.join("tsconfig.json"))? != role.typescript()
    {
        return Err(AuditError::Boundary);
    }
    let manifest: AdapterManifest =
        serde_json::from_str(&read_regular(&directory.join("package.json"))?)?;
    if manifest.name != NAME
        || !manifest.private
        || manifest.kind != "module"
        || manifest.license != LICENSE
        || manifest.exports != "./index.mjs"
    {
        return Err(AuditError::Boundary);
    }
    Ok(())
}

fn local_package(package: &LockedPackage) -> bool {
    package.name.as_deref() == Some(NAME)
        && package.version.is_none()
        && package.resolved.is_none()
        && package.integrity.is_none()
        && package.license.as_deref() == Some(LICENSE)
        && package.dev
        && !package.link
        && package.extra.is_empty()
}

fn local_link(package: &LockedPackage, role: FixtureRole) -> bool {
    package.name.is_none()
        && package.version.is_none()
        && package.resolved.as_deref() == Some(role.entry())
        && package.integrity.is_none()
        && package.license.is_none()
        && !package.dev
        && package.link
        && package.extra.is_empty()
}

pub(super) fn verify(
    root: &Path,
    manifest: &str,
    lock: &Lock,
    role: FixtureRole,
) -> Result<Verified, AuditError> {
    let manifest: RootManifest = serde_json::from_str(manifest)?;
    let resolver = manifest
        .overrides
        .get(RESOLVER)
        .ok_or(AuditError::Boundary)?;
    if manifest.overrides.len() != 1
        || resolver.minimatch != "11.0.9"
        || resolver.tinyglobby != role.override_path()
        || lock
            .packages
            .get("node_modules/@typescript-eslint/typescript-estree")
            .is_none_or(|package| package.version.as_deref() != Some("8.73.0"))
        || !lock.packages.get(role.entry()).is_some_and(local_package)
        || !lock
            .packages
            .get(LINK)
            .is_some_and(|package| local_link(package, role))
    {
        return Err(AuditError::Boundary);
    }
    for (path, package) in &lock.packages {
        if path != LINK
            && (["tinyglobby", "fdir", "picomatch"].iter().any(|name| {
                path == &format!("node_modules/{name}")
                    || path.ends_with(&format!("/node_modules/{name}"))
                    || package.name.as_deref() == Some(*name)
            }))
        {
            return Err(AuditError::Boundary);
        }
    }
    sources(root, role)?;
    Ok(Verified { role })
}
