use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

mod project_service;

const PACKAGES: [(&str, &str); 9] = [
    ("ari-client", "2.2.0"),
    ("cookiejar", "1.3.1"),
    ("form-data", "2.3.3"),
    ("qs", "6.5.5"),
    ("request", "2.88.2"),
    ("shred", "0.8.10"),
    ("swagger-client", "2.0.26"),
    ("tough-cookie", "2.5.0"),
    ("uuid", "3.4.0"),
];
const ADVISORIES: [(&str, &str); 8] = [
    ("cookiejar", "GHSA-h452-7996-h45h"),
    ("form-data", "GHSA-fjxv-7rqg-78g4"),
    ("form-data", "GHSA-hmw2-7cc7-3qxx"),
    ("qs", "GHSA-6rw7-vpxm-498p"),
    ("qs", "GHSA-4mjr-xmp4-gh2g"),
    ("request", "GHSA-p8p7-x288-28g6"),
    ("tough-cookie", "GHSA-72xf-g2v4-qvf3"),
    ("uuid", "GHSA-w5hq-g745-h8pq"),
];

#[derive(Debug)]
pub enum AuditError {
    Json(serde_json::Error),
    Io(std::io::Error),
    Capacity,
    Boundary,
    UnexpectedPackage(String),
    ChangedVersion(String),
    UnreviewedAdvisory(String),
    UnreviewedLicense(String),
    InvalidCause,
    InvalidReport,
}

impl From<serde_json::Error> for AuditError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
impl From<std::io::Error> for AuditError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl std::fmt::Display for AuditError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "invalid dependency audit data: {error}"),
            Self::Io(error) => write!(formatter, "dependency fixture input failed: {error}"),
            Self::Capacity => formatter.write_str("dependency audit capacity exceeded"),
            Self::Boundary => formatter.write_str("dependency fixture boundary changed"),
            Self::UnexpectedPackage(name) => {
                write!(formatter, "unreviewed vulnerable dependency: {name}")
            }
            Self::ChangedVersion(name) => write!(
                formatter,
                "reviewed legacy dependency version changed: {name}"
            ),
            Self::UnreviewedAdvisory(url) => {
                write!(formatter, "unreviewed legacy dependency advisory: {url}")
            }
            Self::UnreviewedLicense(name) => {
                write!(formatter, "unreviewed fixture dependency license: {name}")
            }
            Self::InvalidCause => formatter.write_str("invalid dependency audit cause graph"),
            Self::InvalidReport => formatter.write_str("inconsistent dependency audit report"),
        }
    }
}
impl std::error::Error for AuditError {}

#[derive(Debug, PartialEq, Eq)]
pub struct Assessment {
    pub packages: usize,
    pub advisories: usize,
}

#[derive(Deserialize)]
struct Manifest {
    private: bool,
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(rename = "devDependencies")]
    dev_dependencies: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Lock {
    #[serde(rename = "lockfileVersion")]
    version: u8,
    packages: BTreeMap<String, LockedPackage>,
}
#[derive(Deserialize)]
struct LockedPackage {
    name: Option<String>,
    version: Option<String>,
    resolved: Option<String>,
    integrity: Option<String>,
    license: Option<String>,
    #[serde(default)]
    dev: bool,
    #[serde(default)]
    link: bool,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

fn licenses(lock: &Lock, local: Option<&project_service::Verified>) -> Result<(), AuditError> {
    for (path, package) in &lock.packages {
        if local.is_some_and(|profile| profile.contains(path)) {
            continue;
        }
        if !path.is_empty()
            && (package.link
                || package
                    .resolved
                    .as_ref()
                    .is_none_or(|url| !url.starts_with("https://registry.npmjs.org/"))
                || package.integrity.as_ref().is_none_or(|integrity| {
                    !integrity.starts_with("sha512-")
                        || integrity.len() <= 7
                        || integrity.len() > 128
                }))
        {
            return Err(AuditError::Boundary);
        }
        let known = match package.license.as_deref() {
            Some(
                "MIT"
                | "Apache-2.0"
                | "BSD-2-Clause"
                | "BSD-3-Clause"
                | "ISC"
                | "Unlicense"
                | "BlueOak-1.0.0"
                | "MIT OR Apache-2.0"
                | "(MIT OR Apache-2.0)"
                | "(AFL-2.1 OR BSD-3-Clause)",
            ) => true,
            Some("Apache2") => {
                path == "node_modules/btoa" && package.version.as_deref() == Some("1.1.1")
            }
            Some("apache 2.0") => {
                path == "node_modules/swagger-client"
                    && package.version.as_deref() == Some("2.0.26")
            }
            None => matches!(
                (path.as_str(), package.version.as_deref()),
                ("node_modules/ax", Some("0.1.8"))
                    | ("node_modules/shred", Some("0.8.10"))
                    | ("node_modules/sprintf", Some("0.1.1"))
            ),
            Some(_) => false,
        };
        if !known
            || (!path.is_empty()
                && !package.dev
                && !local.is_some_and(|profile| profile.allows_runtime(path, package)))
        {
            return Err(AuditError::UnreviewedLicense(path.clone()));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Severity {
    Info,
    Low,
    Moderate,
    High,
    Critical,
}

#[derive(Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Counts {
    info: usize,
    low: usize,
    moderate: usize,
    high: usize,
    critical: usize,
    total: usize,
}
impl Counts {
    fn observe(&mut self, severity: Severity) {
        self.total += 1;
        match severity {
            Severity::Info => self.info += 1,
            Severity::Low => self.low += 1,
            Severity::Moderate => self.moderate += 1,
            Severity::High => self.high += 1,
            Severity::Critical => self.critical += 1,
        }
    }
}

#[derive(Deserialize)]
struct Audit {
    #[serde(rename = "auditReportVersion")]
    version: u8,
    vulnerabilities: BTreeMap<String, Vulnerability>,
    metadata: Metadata,
}
#[derive(Deserialize)]
struct Metadata {
    vulnerabilities: Counts,
}
#[derive(Deserialize)]
struct Vulnerability {
    name: String,
    severity: Severity,
    via: Vec<Cause>,
    nodes: Vec<String>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Cause {
    Advisory(Advisory),
    Dependency(String),
}
#[derive(Deserialize)]
struct Advisory {
    name: String,
    dependency: String,
    url: String,
}

#[derive(Clone, Copy)]
enum Visit {
    Unseen,
    Visiting,
    Reviewed,
}

fn index(name: &str) -> Result<usize, AuditError> {
    PACKAGES
        .iter()
        .position(|(package, _)| *package == name)
        .ok_or_else(|| AuditError::UnexpectedPackage(name.into()))
}

fn review<'a>(
    name: &'a str,
    audit: &'a Audit,
    visited: &mut [Visit; 9],
    advisories: &mut BTreeSet<&'a str>,
) -> Result<(), AuditError> {
    let position = index(name)?;
    match visited[position] {
        Visit::Reviewed => return Ok(()),
        Visit::Visiting => return Err(AuditError::InvalidCause),
        Visit::Unseen => visited[position] = Visit::Visiting,
    }
    let vulnerability = audit
        .vulnerabilities
        .get(name)
        .ok_or(AuditError::InvalidCause)?;
    if vulnerability.name != name
        || vulnerability.via.is_empty()
        || vulnerability.via.len() > 16
        || vulnerability.nodes.len() != 1
        || vulnerability
            .nodes
            .first()
            .is_none_or(|node| *node != format!("node_modules/{name}"))
    {
        return Err(AuditError::InvalidCause);
    }
    for cause in &vulnerability.via {
        match cause {
            Cause::Dependency(dependency) => review(dependency, audit, visited, advisories)?,
            Cause::Advisory(advisory) => {
                let id = advisory
                    .url
                    .strip_prefix("https://github.com/advisories/")
                    .ok_or_else(|| AuditError::UnreviewedAdvisory(advisory.url.clone()))?;
                if advisory.name != name
                    || advisory.dependency != name
                    || !ADVISORIES.contains(&(name, id))
                {
                    return Err(AuditError::UnreviewedAdvisory(advisory.url.clone()));
                }
                advisories.insert(id);
            }
        }
    }
    visited[position] = Visit::Reviewed;
    Ok(())
}

fn inputs(manifest: &str, lock: &str, report: &str) -> Result<Lock, AuditError> {
    if [manifest, lock, report]
        .iter()
        .any(|source| source.len() > 1024 * 1024)
    {
        return Err(AuditError::Capacity);
    }
    let manifest: Manifest = serde_json::from_str(manifest)?;
    if !manifest.private
        || !manifest.dependencies.is_empty()
        || manifest
            .dev_dependencies
            .get("ari-client")
            .map(String::as_str)
            != Some("2.2.0")
    {
        return Err(AuditError::Boundary);
    }
    let lock: Lock = serde_json::from_str(lock)?;
    if lock.version != 3 || lock.packages.len() > 512 {
        return Err(AuditError::Boundary);
    }
    Ok(lock)
}

fn assess_lock(
    lock: &Lock,
    report: &str,
    local: Option<&project_service::Verified>,
) -> Result<Assessment, AuditError> {
    for (name, version) in PACKAGES {
        let package = lock
            .packages
            .get(&format!("node_modules/{name}"))
            .ok_or(AuditError::Boundary)?;
        if package.version.as_deref() != Some(version) {
            return Err(AuditError::ChangedVersion(name.into()));
        }
        let resolved = format!("https://registry.npmjs.org/{name}/-/{name}-{version}.tgz");
        if !package.dev
            || package.resolved.as_deref() != Some(resolved.as_str())
            || package
                .integrity
                .as_ref()
                .is_none_or(|integrity| !integrity.starts_with("sha512-") || integrity.len() > 128)
        {
            return Err(AuditError::Boundary);
        }
    }
    licenses(lock, local)?;
    let audit: Audit = serde_json::from_str(report)?;
    if audit.version != 2 || audit.vulnerabilities.len() > 9 {
        return Err(AuditError::InvalidReport);
    }
    let mut counts = Counts::default();
    for vulnerability in audit.vulnerabilities.values() {
        counts.observe(vulnerability.severity);
    }
    if counts != audit.metadata.vulnerabilities {
        return Err(AuditError::InvalidReport);
    }
    let mut visited = [Visit::Unseen; 9];
    let mut advisories = BTreeSet::new();
    for name in audit.vulnerabilities.keys() {
        review(name, &audit, &mut visited, &mut advisories)?;
    }
    Ok(Assessment {
        packages: counts.total,
        advisories: advisories.len(),
    })
}

pub fn assess(manifest: &str, lock: &str, report: &str) -> Result<Assessment, AuditError> {
    let lock = inputs(manifest, lock, report)?;
    assess_lock(&lock, report, None)
}

pub fn assess_fixture(fixture: &Path, report: &str) -> Result<Assessment, AuditError> {
    let manifest = project_service::read_regular(&fixture.join("package.json"))?;
    let lock_text = project_service::read_regular(&fixture.join("package-lock.json"))?;
    let lock = inputs(&manifest, &lock_text, report)?;
    let local = project_service::verify(
        fixture,
        &manifest,
        &lock,
        project_service::FixtureRole::Comparison,
    )?;
    assess_lock(&lock, report, Some(&local))
}

pub fn check_browser_fixture(fixture: &Path) -> Result<(), AuditError> {
    let manifest = project_service::read_regular(&fixture.join("package.json"))?;
    let lock = project_service::read_regular(&fixture.join("package-lock.json"))?;
    let lock: Lock = serde_json::from_str(&lock)?;
    if lock.version != 3 || lock.packages.len() > 512 {
        return Err(AuditError::Boundary);
    }
    let local = project_service::verify(
        fixture,
        &manifest,
        &lock,
        project_service::FixtureRole::Browser,
    )?;
    let manifest: Manifest = serde_json::from_str(&manifest)?;
    if !manifest.private
        || manifest.dependencies.len() != 1
        || manifest.dependencies.get("sip.js").map(String::as_str) != Some("0.21.2")
        || lock
            .packages
            .get("node_modules/sip.js")
            .is_none_or(|package| package.version.as_deref() != Some("0.21.2"))
        || manifest.dev_dependencies.is_empty()
        || manifest.dev_dependencies.iter().any(|(name, version)| {
            semver::Version::parse(version).is_err()
                || lock
                    .packages
                    .get(&format!("node_modules/{name}"))
                    .is_none_or(|package| package.version.as_deref() != Some(version.as_str()))
        })
    {
        return Err(AuditError::Boundary);
    }
    licenses(&lock, Some(&local))
}
