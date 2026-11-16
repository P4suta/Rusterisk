use super::{SdkConsumerPlan, SdkConsumerScope, SdkPackage, SdkRuntimeIdentity};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

#[derive(Debug)]
pub enum SdkMetadataError {
    Size,
    Json(serde_json::Error),
    Contract(&'static str),
}

impl std::fmt::Display for SdkMetadataError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Size => {
                formatter.write_str("SDK consumer metadata exceeds its bounded input profile")
            }
            Self::Json(error) => write!(formatter, "SDK consumer metadata: {error}"),
            Self::Contract(field) => write!(formatter, "SDK consumer metadata: invalid {field}"),
        }
    }
}
impl std::error::Error for SdkMetadataError {}

#[derive(Deserialize)]
struct Metadata {
    version: u8,
    packages: Vec<Package>,
    workspace_root: PathBuf,
    target_directory: PathBuf,
    workspace_members: Vec<String>,
    resolve: Resolve,
}
#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    version: String,
    source: Option<String>,
    manifest_path: PathBuf,
    dependencies: Vec<Dependency>,
    targets: Vec<Target>,
}
#[derive(Deserialize)]
struct Dependency {
    name: String,
    req: String,
    source: Option<String>,
    path: Option<PathBuf>,
    kind: Option<String>,
    target: Option<String>,
    #[serde(default)]
    features: Vec<String>,
    uses_default_features: Option<bool>,
}
#[derive(Deserialize)]
struct Target {
    kind: Vec<String>,
    src_path: PathBuf,
}
#[derive(Deserialize)]
struct Resolve {
    root: Option<String>,
    nodes: Vec<Node>,
}
#[derive(Deserialize)]
struct Node {
    id: String,
    dependencies: Vec<String>,
    deps: Vec<Edge>,
    #[serde(default)]
    features: Vec<String>,
}
#[derive(Deserialize)]
struct Edge {
    name: String,
    pkg: String,
    dep_kinds: Vec<Kind>,
}
#[derive(Deserialize)]
struct Kind {
    kind: Option<String>,
    target: Option<String>,
}

#[derive(Clone, Copy)]
enum Role {
    Consumer,
    Sdk(SdkPackage),
    Registry,
}

fn fail(field: &'static str) -> SdkMetadataError {
    SdkMetadataError::Contract(field)
}
fn string(value: &str) -> bool {
    !value.is_empty() && value.len() <= 16384 && !value.chars().any(char::is_control)
}
fn path(value: &Path) -> bool {
    value.is_absolute()
        && value.to_str().is_some_and(string)
        && !value
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
}
fn registry(source: &str) -> bool {
    string(source)
        && source.split_once('+').is_some_and(|(kind, opaque)| {
            !opaque.is_empty() && ["registry", "sparse"].contains(&kind)
        })
}
fn selected(scope: SdkConsumerScope) -> &'static [SdkPackage] {
    match scope {
        SdkConsumerScope::CoreMsrv => &[SdkPackage::Core],
        SdkConsumerScope::Sdk => &SdkPackage::ALL,
    }
}

fn role(package: &Package, scope: SdkConsumerScope) -> Result<Role, SdkMetadataError> {
    if package.name == scope.package_name() {
        return Ok(Role::Consumer);
    }
    if let Some(sdk) = selected(scope)
        .iter()
        .find(|sdk| sdk.name() == package.name)
    {
        return Ok(Role::Sdk(*sdk));
    }
    if package.name.starts_with("rusterisk-")
        || package
            .source
            .as_deref()
            .is_none_or(|source| !registry(source))
    {
        return Err(fail("backend or unselected local package"));
    }
    Ok(Role::Registry)
}

fn package_path(plan: &SdkConsumerPlan, package: SdkPackage, version: &str) -> PathBuf {
    plan.work()
        .join("packages")
        .join(format!("{}-{version}", package.name()))
}

fn check_package(
    package: &Package,
    role: Role,
    plan: &SdkConsumerPlan,
    scope: SdkConsumerScope,
    version: &str,
) -> Result<(), SdkMetadataError> {
    if !string(&package.id)
        || !string(&package.name)
        || !path(&package.manifest_path)
        || semver::Version::parse(&package.version).is_err()
        || package.dependencies.len() > 256
        || package.targets.is_empty()
        || package.targets.len() > 512
    {
        return Err(fail("package fields"));
    }
    let directory = match role {
        Role::Consumer => {
            if package.version != "0.0.0" || package.source.is_some() {
                return Err(fail("consumer identity"));
            }
            plan.directory(scope).to_owned()
        }
        Role::Sdk(sdk) => {
            if package.version != version || package.source.is_some() {
                return Err(fail("candidate package identity"));
            }
            if sdk == SdkPackage::Core && !package.dependencies.is_empty() {
                return Err(fail("dependency-free core"));
            }
            package_path(plan, sdk, version)
        }
        Role::Registry => package
            .manifest_path
            .parent()
            .ok_or_else(|| fail("registry manifest path"))?
            .to_owned(),
    };
    if package.manifest_path != directory.join("Cargo.toml") {
        return Err(fail("extracted manifest path"));
    }
    for target in &package.targets {
        if !path(&target.src_path)
            || !target.src_path.starts_with(&directory)
            || target.kind.is_empty()
            || target.kind.len() > 16
            || target.kind.iter().any(|kind| !string(kind))
        {
            return Err(fail("target path"));
        }
    }
    match role {
        Role::Consumer => {
            if package.targets.len() != 1
                || package.targets[0].kind != ["bin"]
                || package.targets[0].src_path != directory.join("src/main.rs")
            {
                return Err(fail("consumer target"));
            }
        }
        Role::Sdk(_) => {
            if !package.targets.iter().any(|target| {
                target.kind == ["lib"] && target.src_path == directory.join("src/lib.rs")
            }) {
                return Err(fail("SDK library target"));
            }
        }
        Role::Registry => {}
    }
    for dependency in &package.dependencies {
        if !string(&dependency.name)
            || semver::VersionReq::parse(&dependency.req).is_err()
            || dependency.path.is_some()
            || dependency
                .source
                .as_deref()
                .is_some_and(|source| !registry(source))
            || !matches!(dependency.kind.as_deref(), None | Some("dev" | "build"))
            || dependency
                .target
                .as_deref()
                .is_some_and(|target| !string(target))
            || dependency.features.len() > 256
            || dependency.features.iter().any(|feature| !string(feature))
        {
            return Err(fail("dependency source"));
        }
    }
    Ok(())
}

fn normal_edge(node: &Node, package: &Package) -> bool {
    node.dependencies.contains(&package.id)
        && node.deps.iter().any(|edge| {
            edge.pkg == package.id
                && edge.name == package.name.replace('-', "_")
                && edge
                    .dep_kinds
                    .iter()
                    .any(|kind| kind.kind.is_none() && kind.target.is_none())
        })
}

fn declared(package: &Package, sdk: SdkPackage, requirement: &semver::VersionReq) -> bool {
    package
        .dependencies
        .iter()
        .filter(|dependency| {
            dependency.name == sdk.name()
                && dependency.kind.is_none()
                && dependency.target.is_none()
        })
        .count()
        == 1
        && package.dependencies.iter().any(|dependency| {
            dependency.name == sdk.name()
                && dependency.kind.is_none()
                && dependency.target.is_none()
                && semver::VersionReq::parse(&dependency.req)
                    .is_ok_and(|actual| actual == *requirement)
        })
}

fn check(
    plan: &SdkConsumerPlan,
    scope: SdkConsumerScope,
    version: &str,
    bytes: &[u8],
    runtime: Option<&SdkRuntimeIdentity>,
) -> Result<(), SdkMetadataError> {
    if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
        return Err(SdkMetadataError::Size);
    }
    let version_value = semver::Version::parse(version).map_err(|_| fail("candidate version"))?;
    crate::regular_archive::ArchiveRoot::new(&format!("rusterisk-core-{version}"))
        .map_err(|_| fail("candidate version"))?;
    let metadata: Metadata = serde_json::from_slice(bytes).map_err(SdkMetadataError::Json)?;
    if metadata.packages.is_empty()
        || metadata.packages.len() > 1024
        || metadata.resolve.nodes.len() > 1024
    {
        return Err(SdkMetadataError::Size);
    }
    if metadata.version != 1
        || metadata.workspace_root != plan.directory(scope)
        || metadata.target_directory != plan.target(scope)
    {
        return Err(fail("workspace identity"));
    }
    let mut packages = BTreeMap::new();
    let mut sdk = BTreeMap::new();
    let mut consumer = None;
    for package in &metadata.packages {
        let role = role(package, scope)?;
        check_package(package, role, plan, scope, version)?;
        if packages.insert(package.id.as_str(), package).is_some() {
            return Err(fail("duplicate package ID"));
        }
        match role {
            Role::Consumer => {
                if consumer.replace(package).is_some() {
                    return Err(fail("duplicate consumer"));
                }
            }
            Role::Sdk(_) => {
                if sdk.insert(package.name.as_str(), package).is_some() {
                    return Err(fail("duplicate SDK package"));
                }
            }
            Role::Registry => {}
        }
    }
    let consumer = consumer.ok_or_else(|| fail("consumer package"))?;
    if sdk.len() != selected(scope).len()
        || metadata.workspace_members != [consumer.id.clone()]
        || metadata.resolve.root.as_deref() != Some(consumer.id.as_str())
        || (scope == SdkConsumerScope::CoreMsrv && packages.len() != 2)
    {
        return Err(fail("consumer graph root"));
    }
    let mut nodes = BTreeMap::new();
    for node in &metadata.resolve.nodes {
        if !packages.contains_key(node.id.as_str())
            || nodes.insert(node.id.as_str(), node).is_some()
            || node.dependencies.len() > 256
            || node.deps.len() > 256
            || node.features.len() > 256
            || node.features.iter().any(|feature| !string(feature))
        {
            return Err(fail("resolved package graph"));
        }
        let dependencies: BTreeSet<_> = node.dependencies.iter().map(String::as_str).collect();
        let edges: BTreeSet<_> = node.deps.iter().map(|edge| edge.pkg.as_str()).collect();
        if dependencies.len() != node.dependencies.len()
            || dependencies != edges
            || dependencies
                .iter()
                .any(|dependency| !packages.contains_key(*dependency))
        {
            return Err(fail("resolved dependency IDs"));
        }
        for edge in &node.deps {
            if !string(&edge.name)
                || edge.dep_kinds.is_empty()
                || edge.dep_kinds.len() > 8
                || edge.dep_kinds.iter().any(|kind| {
                    !matches!(kind.kind.as_deref(), None | Some("dev" | "build"))
                        || kind.target.as_deref().is_some_and(|target| !string(target))
                })
            {
                return Err(fail("resolved dependency kinds"));
            }
        }
    }
    if nodes.len() != packages.len() {
        return Err(fail("complete dependency graph"));
    }
    let root = nodes
        .get(consumer.id.as_str())
        .ok_or_else(|| fail("consumer node"))?;
    let expected = selected(scope).len() + usize::from(runtime.is_some());
    if consumer.dependencies.len() != expected
        || root.dependencies.len() != expected
        || root.deps.len() != expected
    {
        return Err(fail("consumer direct dependency set"));
    }
    let exact =
        semver::VersionReq::parse(&format!("={version}")).map_err(|_| fail("candidate version"))?;
    for selected in selected(scope) {
        let package = sdk
            .get(selected.name())
            .ok_or_else(|| fail("selected SDK package"))?;
        if !declared(consumer, *selected, &exact) || !normal_edge(root, package) {
            return Err(fail("normal consumer SDK dependency"));
        }
    }
    if scope == SdkConsumerScope::CoreMsrv && root.dependencies.len() != 1 {
        return Err(fail("isolated core consumer"));
    }
    if scope == SdkConsumerScope::Sdk {
        let client = sdk
            .get("rusterisk-client")
            .ok_or_else(|| fail("client package"))?;
        let node = nodes
            .get(client.id.as_str())
            .ok_or_else(|| fail("client node"))?;
        for peer in [SdkPackage::Core, SdkPackage::Api] {
            let package = sdk
                .get(peer.name())
                .ok_or_else(|| fail("client peer package"))?;
            if !normal_edge(node, package)
                || !client.dependencies.iter().any(|dependency| {
                    dependency.name == peer.name()
                        && dependency.kind.is_none()
                        && dependency.target.is_none()
                        && semver::VersionReq::parse(&dependency.req)
                            .is_ok_and(|requirement| requirement.matches(&version_value))
                })
            {
                return Err(fail("same-candidate client peer"));
            }
        }
    }
    if let Some(runtime) = runtime {
        let mut candidates = metadata
            .packages
            .iter()
            .filter(|package| package.name == "tokio");
        let package = candidates.next().ok_or_else(|| fail("candidate runtime"))?;
        if candidates.next().is_some()
            || package.version != runtime.version()
            || package.source.as_deref() != Some(runtime.source())
            || !normal_edge(root, package)
        {
            return Err(fail("candidate runtime identity"));
        }
        let declaration = consumer
            .dependencies
            .iter()
            .find(|dependency| dependency.name == "tokio")
            .ok_or_else(|| fail("runtime declaration"))?;
        let exact = semver::VersionReq::parse(&format!("={}", runtime.version()))
            .map_err(|_| fail("runtime version"))?;
        let features: BTreeSet<_> = declaration.features.iter().map(String::as_str).collect();
        if declaration.kind.is_some()
            || declaration.target.is_some()
            || declaration.source.as_deref() != Some(runtime.source())
            || declaration.uses_default_features != Some(false)
            || declaration.features.len() != 3
            || features != BTreeSet::from(["net", "rt", "time"])
            || !semver::VersionReq::parse(&declaration.req)
                .is_ok_and(|requirement| requirement == exact)
        {
            return Err(fail("runtime dependency contract"));
        }
        let node = nodes
            .get(package.id.as_str())
            .ok_or_else(|| fail("runtime node"))?;
        if ["net", "rt", "time"]
            .iter()
            .any(|feature| !node.features.iter().any(|actual| actual == feature))
        {
            return Err(fail("resolved runtime features"));
        }
    }
    Ok(())
}

pub fn check_sdk_consumer_metadata(
    plan: &SdkConsumerPlan,
    scope: SdkConsumerScope,
    version: &str,
    bytes: &[u8],
) -> Result<(), SdkMetadataError> {
    check(plan, scope, version, bytes, None)
}

pub fn check_sdk_http_metadata(
    plan: &SdkConsumerPlan,
    version: &str,
    runtime: &SdkRuntimeIdentity,
    bytes: &[u8],
) -> Result<(), SdkMetadataError> {
    check(plan, SdkConsumerScope::Sdk, version, bytes, Some(runtime))
}
