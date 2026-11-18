use super::{SdkConsumerPlan, SdkConsumerScope, SdkPackage, SdkRuntimeIdentity};
use crate::recorded_path::{RecordedPath, RecordedPathFormat};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
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
fn path(format: RecordedPathFormat, value: &Path) -> Result<RecordedPath, SdkMetadataError> {
    let value = value.to_str().ok_or_else(|| fail("absolute path"))?;
    let normalized;
    let value = match format {
        RecordedPathFormat::Windows
            if value
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
                && value.as_bytes().get(1) == Some(&b':') =>
        {
            normalized = value.replace('/', "\\");
            normalized.as_str()
        }
        RecordedPathFormat::Unix | RecordedPathFormat::Windows => value,
    };
    format
        .parse(Path::new(value), 16384)
        .map_err(|_| fail("absolute path"))
}

struct MetadataPaths {
    format: RecordedPathFormat,
    work: RecordedPath,
    directory: RecordedPath,
    target: RecordedPath,
}

impl MetadataPaths {
    fn live(plan: &SdkConsumerPlan, scope: SdkConsumerScope) -> Result<Self, SdkMetadataError> {
        #[cfg(windows)]
        let format = RecordedPathFormat::Windows;
        #[cfg(not(windows))]
        let format = RecordedPathFormat::Unix;
        Ok(Self {
            format,
            work: path(format, plan.work())?,
            directory: path(format, plan.directory(scope))?,
            target: path(format, plan.target(scope))?,
        })
    }

    fn recorded(format: RecordedPathFormat, root: &RecordedPath, scope: SdkConsumerScope) -> Self {
        Self {
            format,
            work: root.join("work"),
            directory: root.join(&format!("work/consumers/{}", scope.label())),
            target: root.join(&format!("state/target/{}", scope.label())),
        }
    }
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

fn package_path(plan: &MetadataPaths, package: SdkPackage, version: &str) -> RecordedPath {
    plan.work
        .join(&format!("packages/{}-{version}", package.name()))
}

fn check_package(
    package: &Package,
    role: Role,
    plan: &MetadataPaths,
    version: &str,
) -> Result<(), SdkMetadataError> {
    if !string(&package.id)
        || !string(&package.name)
        || semver::Version::parse(&package.version).is_err()
        || package.dependencies.len() > 256
        || package.targets.is_empty()
        || package.targets.len() > 512
    {
        return Err(fail("package fields"));
    }
    let manifest_path =
        path(plan.format, &package.manifest_path).map_err(|_| fail("package fields"))?;
    let directory = match role {
        Role::Consumer => {
            if package.version != "0.0.0" || package.source.is_some() {
                return Err(fail("consumer identity"));
            }
            plan.directory.clone()
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
        Role::Registry => manifest_path
            .parent()
            .ok_or_else(|| fail("registry manifest path"))?,
    };
    if manifest_path != directory.join("Cargo.toml") {
        return Err(fail("extracted manifest path"));
    }
    for target in &package.targets {
        let source = path(plan.format, &target.src_path).map_err(|_| fail("target path"))?;
        if !source.starts_with(&directory)
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
                || path(plan.format, &package.targets[0].src_path)? != directory.join("src/main.rs")
            {
                return Err(fail("consumer target"));
            }
        }
        Role::Sdk(_) => {
            if !package.targets.iter().any(|target| {
                target.kind == ["lib"]
                    && path(plan.format, &target.src_path)
                        .is_ok_and(|source| source == directory.join("src/lib.rs"))
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
    plan: &MetadataPaths,
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
        || path(plan.format, &metadata.workspace_root).map_err(|_| fail("workspace identity"))?
            != plan.directory
        || path(plan.format, &metadata.target_directory).map_err(|_| fail("workspace identity"))?
            != plan.target
    {
        return Err(fail("workspace identity"));
    }
    let mut packages = BTreeMap::new();
    let mut sdk = BTreeMap::new();
    let mut consumer = None;
    for package in &metadata.packages {
        let role = role(package, scope)?;
        check_package(package, role, plan, version)?;
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
    check(
        &MetadataPaths::live(plan, scope)?,
        scope,
        version,
        bytes,
        None,
    )
}

pub fn check_sdk_http_metadata(
    plan: &SdkConsumerPlan,
    version: &str,
    runtime: &SdkRuntimeIdentity,
    bytes: &[u8],
) -> Result<(), SdkMetadataError> {
    check(
        &MetadataPaths::live(plan, SdkConsumerScope::Sdk)?,
        SdkConsumerScope::Sdk,
        version,
        bytes,
        Some(runtime),
    )
}

pub(super) fn check_recorded(
    format: RecordedPathFormat,
    consumer_root: &RecordedPath,
    scope: SdkConsumerScope,
    version: &str,
    bytes: &[u8],
    runtime: Option<&SdkRuntimeIdentity>,
) -> Result<(), SdkMetadataError> {
    check(
        &MetadataPaths::recorded(format, consumer_root, scope),
        scope,
        version,
        bytes,
        runtime,
    )
}

#[cfg(test)]
mod recorded_tests {
    use super::{SdkConsumerPlan, SdkConsumerScope};
    use crate::recorded_path::{
        RecordedPath,
        RecordedPathFormat::{self, Unix, Windows},
    };
    use http_fixture::runtime;
    use serde_json::{json, Value};
    use std::{
        io,
        path::{Path, PathBuf},
    };

    mod metadata_fixture {
        use crate as xtask;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/sdk_consumer_metadata/fixture.rs"
        ));
    }

    mod http_fixture {
        use crate as xtask;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/sdk_http_metadata/fixture.rs"
        ));
    }

    fn array(values: impl IntoIterator<Item = Value>) -> Value {
        Value::Array(values.into_iter().collect())
    }

    fn path(root: &str, relative: &str) -> String {
        if root.starts_with('/') {
            format!("{root}/{relative}")
        } else {
            format!("{root}\\{}", relative.replace('/', "\\"))
        }
    }

    fn projected_path(plan: &SdkConsumerPlan, root: &str, value: &Path) -> io::Result<PathBuf> {
        let (prefix, relative) = if let Ok(relative) = value.strip_prefix(plan.work()) {
            ("work", relative)
        } else if let Ok(relative) = value.strip_prefix(plan.state()) {
            ("state", relative)
        } else {
            let relative = value
                .strip_prefix(metadata_fixture::checkout(plan))
                .map_err(io::Error::other)?;
            ("checkout", relative)
        };
        let relative = relative
            .to_str()
            .ok_or_else(|| io::Error::other("fixture path is not UTF-8"))?;
        Ok(PathBuf::from(path(
            root,
            &format!("{prefix}/{}", relative.replace('\\', "/")),
        )))
    }

    fn projected_metadata(
        plan: &SdkConsumerPlan,
        root: &str,
        mut data: metadata_fixture::Metadata,
        with_runtime: bool,
    ) -> io::Result<Value> {
        data.workspace_root = projected_path(plan, root, &data.workspace_root)?;
        data.target_directory = projected_path(plan, root, &data.target_directory)?;
        for package in &mut data.packages {
            package.manifest_path = projected_path(plan, root, &package.manifest_path)?;
            for target in &mut package.targets {
                target.src_path = projected_path(plan, root, &target.src_path)?;
            }
        }
        let bytes = if with_runtime {
            http_fixture::bytes(&data, 0)?
        } else {
            data.bytes()?
        };
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }

    fn metadata(root: &str, scope: SdkConsumerScope, with_runtime: bool) -> io::Result<Value> {
        let plan = metadata_fixture::plan()?;
        let data = if with_runtime {
            http_fixture::data(&plan)?
        } else {
            metadata_fixture::metadata(&plan, scope)
        };
        projected_metadata(&plan, root, data, with_runtime)
    }

    fn bytes(value: &Value) -> io::Result<Vec<u8>> {
        serde_json::to_vec(value).map_err(io::Error::other)
    }

    fn check(
        format: RecordedPathFormat,
        root: &RecordedPath,
        scope: SdkConsumerScope,
        data: &Value,
        with_runtime: bool,
    ) -> io::Result<()> {
        let runtime = if with_runtime { Some(runtime()?) } else { None };
        super::check_recorded(
            format,
            root,
            scope,
            "0.1.0",
            &bytes(data)?,
            runtime.as_ref(),
        )
        .map_err(io::Error::other)
    }

    #[test]
    fn windows_recorded_metadata_accepts_the_owned_candidate_graph() -> io::Result<()> {
        let root = Windows.parse(Path::new(r"\\?\D:\Run\Consumer"), 4096)?;
        for scope in SdkConsumerScope::ALL {
            for source in [r"d:\RUN\CONSUMER", r"\\?\D:\Run\Consumer"] {
                let data = metadata(source, scope, false)?;
                check(Windows, &root, scope, &data, false)?;
            }
        }
        let mut data = metadata(r"d:\run\consumer", SdkConsumerScope::Sdk, true)?;
        data["packages"][0]["targets"][0]["src_path"] =
            json!(r"d:\run\consumer\work\packages\rusterisk-core-0.1.0\src/lib.rs");
        data["workspace_root"] = json!("d:/run/consumer/work/consumers/sdk");
        check(Windows, &root, SdkConsumerScope::Sdk, &data, true)
    }

    #[test]
    fn windows_recorded_metadata_rejects_escaped_or_ambiguous_paths() -> io::Result<()> {
        let root = Windows.parse(Path::new(r"D:\Run\Consumer"), 4096)?;
        for source in [
            r"D:\Run\Consumer\work-copy\packages\rusterisk-core-0.1.0\src\lib.rs",
            r"C:\Run\Consumer\work\packages\rusterisk-core-0.1.0\src\lib.rs",
            r"D:\Run\Consumer\work\packages\rusterisk-core-0.1.0\..\rusterisk-core-0.1.0\src\lib.rs",
            r"D:\Run\Consumer\work\packages\rusterisk-core-0.1.0\src\lib.rs:stream",
            r"D:\Run\Consumer\work\packages\rusterisk-core-0.1.0\src\lib.rs.",
            r"\\host\share\work\packages\rusterisk-core-0.1.0\src\lib.rs",
            r"\\?\D:\Run\Consumer\work\packages\rusterisk-core-0.1.0\src/lib.rs",
        ] {
            let mut data = metadata(r"D:\Run\Consumer", SdkConsumerScope::Sdk, true)?;
            data["packages"][0]["targets"][0]["src_path"] = json!(source);
            assert!(
                check(Windows, &root, SdkConsumerScope::Sdk, &data, true).is_err(),
                "{source}"
            );
        }
        Ok(())
    }

    #[test]
    fn recorded_metadata_preserves_candidate_source_version_and_graph_checks() -> io::Result<()> {
        let root = Windows.parse(Path::new(r"D:\Run\Consumer"), 4096)?;
        for (pointer, replacement) in [
            ("/packages/0/version", json!("0.2.0")),
            ("/packages/0/source", json!("registry+opaque-source")),
            (
                "/packages/2/dependencies/0/path",
                json!(r"D:\Checkout\crates\core"),
            ),
            ("/resolve/nodes/3/deps/0/dep_kinds/0/kind", json!("dev")),
            ("/packages/4/version", json!("1.54.0")),
            ("/packages/4/source", json!("registry+another-source")),
            (
                "/packages/3/dependencies/3/uses_default_features",
                json!(true),
            ),
            ("/resolve/nodes/4/features", array([json!("rt")])),
        ] {
            let mut data = metadata(r"D:\Run\Consumer", SdkConsumerScope::Sdk, true)?;
            *data
                .pointer_mut(pointer)
                .ok_or_else(|| io::Error::other("fixture field missing"))? = replacement;
            assert!(
                check(Windows, &root, SdkConsumerScope::Sdk, &data, true).is_err(),
                "{pointer}"
            );
        }
        Ok(())
    }

    #[test]
    fn unix_recorded_metadata_keeps_case_sensitive_workspace_paths() -> io::Result<()> {
        let root = Unix.parse(Path::new("/Run/Consumer"), 4096)?;
        let mut data = metadata("/Run/Consumer", SdkConsumerScope::Sdk, true)?;
        check(Unix, &root, SdkConsumerScope::Sdk, &data, true)?;
        data["workspace_root"] = json!("/run/consumer/work/consumers/sdk");
        assert!(check(Unix, &root, SdkConsumerScope::Sdk, &data, true).is_err());
        Ok(())
    }

    #[test]
    fn recorded_metadata_rejects_checkout_manifests_and_backend_packages() -> io::Result<()> {
        let root = Windows.parse(Path::new(r"D:\Run\Consumer"), 4096)?;
        let plan = metadata_fixture::plan()?;
        for backend in [false, true] {
            let mut data = metadata_fixture::metadata(&plan, SdkConsumerScope::Sdk);
            if backend {
                data.add_external(&plan, 0)?;
                data.package("registry-package-0")?.name = "rusterisk-server".to_owned();
            } else {
                data.package("rusterisk-core")?.manifest_path =
                    metadata_fixture::checkout(&plan).join("crates/core/Cargo.toml");
            }
            let data = projected_metadata(&plan, r"D:\Run\Consumer", data, false)?;
            assert!(
                check(Windows, &root, SdkConsumerScope::Sdk, &data, false).is_err(),
                "backend {backend}"
            );
        }
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn live_windows_metadata_uses_the_native_pathbuf_join_rules() -> io::Result<()> {
        for (root, expected) in [
            (
                r"D:\Run\Consumer",
                r"D:\Run\Consumer\work\packages\rusterisk-core-0.1.0\src/lib.rs",
            ),
            (
                r"\\?\D:\Run\Consumer",
                r"\\?\D:\Run\Consumer\work\packages\rusterisk-core-0.1.0\src\lib.rs",
            ),
        ] {
            let execution = Path::new(root);
            let plan =
                super::SdkConsumerPlan::new(&execution.join("work"), &execution.join("state"))?;
            let source = plan
                .work()
                .join("packages")
                .join("rusterisk-core-0.1.0")
                .join("src/lib.rs");
            assert_eq!(source.to_str(), Some(expected));
            let mut data = metadata(root, SdkConsumerScope::Sdk, false)?;
            data["packages"][0]["targets"][0]["src_path"] = json!(source);
            super::check_sdk_consumer_metadata(
                &plan,
                SdkConsumerScope::Sdk,
                "0.1.0",
                &bytes(&data)?,
            )
            .map_err(io::Error::other)?;
        }
        Ok(())
    }
}
