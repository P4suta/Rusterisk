use super::{sdk, SdkInputError};
use std::{collections::BTreeSet, path::Path};

#[derive(Debug)]
pub enum DependencyPathError {
    Input(SdkInputError),
    Shape {
        manifest: String,
    },
    Inventory,
    Unversioned {
        manifest: String,
        dependency: String,
    },
    Inheritance {
        manifest: String,
        dependency: String,
    },
}

impl std::fmt::Display for DependencyPathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(error) => error.fmt(formatter),
            Self::Shape { manifest } => write!(formatter, "{manifest}: invalid dependency table"),
            Self::Inventory => formatter.write_str("Cargo.toml: require at most 64 distinct literal workspace members"),
            Self::Unversioned { manifest, dependency } => write!(formatter, "{manifest}: {dependency} requires a version outside the three client development paths"),
            Self::Inheritance { manifest, dependency } => write!(formatter, "{manifest}: invalid workspace inheritance for {dependency}"),
        }
    }
}

impl std::error::Error for DependencyPathError {}

impl From<SdkInputError> for DependencyPathError {
    fn from(error: SdkInputError) -> Self {
        Self::Input(error)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Kind {
    Normal,
    Development,
    Build,
    Workspace,
}

impl Kind {
    const PACKAGE: [(Self, &'static str); 3] = [
        (Self::Normal, "dependencies"),
        (Self::Development, "dev-dependencies"),
        (Self::Build, "build-dependencies"),
    ];
}

fn parse(path: &str, bytes: &[u8]) -> Result<toml::Value, DependencyPathError> {
    if bytes.is_empty() || bytes.len() > sdk::MAX_INPUT {
        return Err(SdkInputError::Size {
            path: path.to_owned(),
        }
        .into());
    }
    Ok(sdk::manifest(path, bytes)?)
}

fn workspace_dependency<'a>(workspace: &'a toml::Value, name: &str) -> Option<&'a toml::Value> {
    workspace.get("workspace")?.get("dependencies")?.get(name)
}

struct Location<'a> {
    path: &'a str,
    package: Option<&'a str>,
    kind: Kind,
    target: bool,
}

fn allowed_development_path(location: &Location<'_>, name: &str, dependency: &toml::Table) -> bool {
    if location.path != "crates/client/Cargo.toml"
        || location.package != Some("rusterisk-client")
        || location.kind != Kind::Development
        || location.target
        || dependency.len() != 1
    {
        return false;
    }
    let expected = match name {
        "rusterisk-server" => "../server",
        "rusterisk-config" => "../config",
        "rusterisk-media" => "../media",
        _ => return false,
    };
    dependency.get("path").and_then(toml::Value::as_str) == Some(expected)
}

fn dependency(
    location: &Location<'_>,
    name: &str,
    value: &toml::Value,
    workspace: &toml::Value,
) -> Result<(), DependencyPathError> {
    if value.is_str() {
        return Ok(());
    }
    let table = value.as_table().ok_or_else(|| DependencyPathError::Shape {
        manifest: location.path.to_owned(),
    })?;
    if let Some(inheritance) = table.get("workspace") {
        if inheritance.as_bool() != Some(true)
            || location.kind == Kind::Workspace
            || ["version", "path", "git"]
                .iter()
                .any(|field| table.contains_key(*field))
        {
            return Err(DependencyPathError::Inheritance {
                manifest: location.path.to_owned(),
                dependency: name.to_owned(),
            });
        }
        let inherited = workspace_dependency(workspace, name).ok_or_else(|| {
            DependencyPathError::Inheritance {
                manifest: location.path.to_owned(),
                dependency: name.to_owned(),
            }
        })?;
        let authority = Location {
            kind: Kind::Workspace,
            ..*location
        };
        return dependency(&authority, name, inherited, workspace);
    }
    if !table.contains_key("path") && !table.contains_key("git") {
        return Ok(());
    }
    if table
        .get("version")
        .and_then(toml::Value::as_str)
        .is_some_and(|value| {
            semver::VersionReq::parse(value)
                .is_ok_and(|requirement| requirement != semver::VersionReq::STAR)
        })
        || allowed_development_path(location, name, table)
    {
        return Ok(());
    }
    Err(DependencyPathError::Unversioned {
        manifest: location.path.to_owned(),
        dependency: name.to_owned(),
    })
}

fn table(
    location: &Location<'_>,
    value: &toml::Value,
    workspace: &toml::Value,
) -> Result<(), DependencyPathError> {
    let entries = value.as_table().ok_or_else(|| DependencyPathError::Shape {
        manifest: location.path.to_owned(),
    })?;
    for (name, value) in entries {
        dependency(location, name, value, workspace)?;
    }
    Ok(())
}

fn inspect(
    path: &str,
    source: &toml::Value,
    inherited_workspace: &toml::Value,
) -> Result<(), DependencyPathError> {
    let workspace = if source.get("workspace").is_some() {
        source
    } else {
        inherited_workspace
    };
    let package = source
        .get("package")
        .and_then(|value| value.get("name"))
        .and_then(toml::Value::as_str);
    if let Some(dependencies) = source
        .get("workspace")
        .and_then(|value| value.get("dependencies"))
    {
        table(
            &Location {
                path,
                package,
                kind: Kind::Workspace,
                target: false,
            },
            dependencies,
            workspace,
        )?;
    }
    for (kind, name) in Kind::PACKAGE {
        if let Some(dependencies) = source.get(name) {
            table(
                &Location {
                    path,
                    package,
                    kind,
                    target: false,
                },
                dependencies,
                workspace,
            )?;
        }
    }
    if let Some(targets) = source.get("target") {
        let targets = targets
            .as_table()
            .ok_or_else(|| DependencyPathError::Shape {
                manifest: path.to_owned(),
            })?;
        for target in targets.values() {
            let target = target
                .as_table()
                .ok_or_else(|| DependencyPathError::Shape {
                    manifest: path.to_owned(),
                })?;
            for (kind, name) in Kind::PACKAGE {
                if let Some(dependencies) = target.get(name) {
                    table(
                        &Location {
                            path,
                            package,
                            kind,
                            target: true,
                        },
                        dependencies,
                        workspace,
                    )?;
                }
            }
        }
    }
    Ok(())
}

pub fn validate_dependency_manifest(
    path: &str,
    source: &str,
    workspace: &str,
) -> Result<(), DependencyPathError> {
    let workspace = parse("Cargo.toml", workspace.as_bytes())?;
    inspect("Cargo.toml", &workspace, &workspace)?;
    inspect(path, &parse(path, source.as_bytes())?, &workspace)
}

pub fn check_dependency_paths_with(
    mut read: impl FnMut(&str) -> Result<Vec<u8>, SdkInputError>,
) -> Result<usize, DependencyPathError> {
    let workspace = parse("Cargo.toml", &read("Cargo.toml")?)?;
    inspect("Cargo.toml", &workspace, &workspace)?;
    let members = workspace
        .get("workspace")
        .and_then(|value| value.get("members"))
        .and_then(toml::Value::as_array)
        .ok_or(DependencyPathError::Inventory)?;
    if members.len() > 64 {
        return Err(DependencyPathError::Inventory);
    }
    let mut paths = BTreeSet::new();
    for member in members {
        let member = member.as_str().ok_or(DependencyPathError::Inventory)?;
        if member.len() > 240
            || member.contains(['\\', ':', '*', '?', '[', ']', '{', '}'])
            || member
                .split('/')
                .any(|value| value.is_empty() || value == "." || value == "..")
            || !paths.insert(format!("{member}/Cargo.toml"))
        {
            return Err(DependencyPathError::Inventory);
        }
    }
    paths.extend(
        [
            "xtask/fuzz/Cargo.toml",
            "crates/core/assurance/Cargo.toml",
            "crates/core/proofs/Cargo.toml",
        ]
        .map(str::to_owned),
    );
    for path in &paths {
        inspect(path, &parse(path, &read(path)?)?, &workspace)?;
    }
    Ok(paths.len() + 1)
}

pub fn check_dependency_paths(root: &Path) -> Result<usize, DependencyPathError> {
    check_dependency_paths_with(|path| sdk::read(root, path))
}
