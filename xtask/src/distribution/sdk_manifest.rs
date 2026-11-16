use std::collections::BTreeSet;

use super::{sdk::MAX_INPUT, SdkPackage};
use toml::{Table, Value};

#[derive(Debug)]
pub enum SdkManifestError {
    Size,
    Syntax(toml::de::Error),
    Contract(&'static str),
}

impl std::fmt::Display for SdkManifestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Size => write!(
                formatter,
                "SDK manifests require 1 through {MAX_INPUT} bytes"
            ),
            Self::Syntax(source) => write!(formatter, "SDK manifest: {source}"),
            Self::Contract(field) => write!(formatter, "SDK normalized manifest: invalid {field}"),
        }
    }
}

impl std::error::Error for SdkManifestError {}

fn parse(source: &str) -> Result<Value, SdkManifestError> {
    if source.is_empty() || source.len() > MAX_INPUT {
        return Err(SdkManifestError::Size);
    }
    toml::from_str(source).map_err(SdkManifestError::Syntax)
}

fn table<'a>(value: Option<&'a Value>, field: &'static str) -> Result<&'a Table, SdkManifestError> {
    value
        .and_then(Value::as_table)
        .ok_or(SdkManifestError::Contract(field))
}

fn keys(source: &Table, allowed: &[&str], field: &'static str) -> Result<(), SdkManifestError> {
    if source.keys().all(|key| allowed.contains(&key.as_str())) {
        Ok(())
    } else {
        Err(SdkManifestError::Contract(field))
    }
}

fn inherited<'a>(
    original: &'a Table,
    workspace: &'a Table,
    field: &'static str,
) -> Result<&'a str, SdkManifestError> {
    let original = original
        .get(field)
        .ok_or(SdkManifestError::Contract(field))?;
    let value = if let Some(inheritance) = original.as_table() {
        if inheritance.len() != 1
            || inheritance.get("workspace").and_then(Value::as_bool) != Some(true)
        {
            return Err(SdkManifestError::Contract(field));
        }
        workspace.get(field)
    } else {
        Some(original)
    };
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(SdkManifestError::Contract(field))
}

fn metadata(
    package: SdkPackage,
    original: &Value,
    workspace: &Value,
    published: &Value,
) -> Result<(), SdkManifestError> {
    let original = table(original.get("package"), "source package")?;
    let workspace = table(
        workspace
            .get("workspace")
            .and_then(|value| value.get("package")),
        "workspace.package",
    )?;
    let published = table(published.get("package"), "package")?;
    keys(
        published,
        &[
            "name",
            "version",
            "edition",
            "rust-version",
            "license",
            "repository",
            "description",
            "readme",
            "exclude",
            "build",
            "autolib",
            "autobins",
            "autoexamples",
            "autotests",
            "autobenches",
        ],
        "package fields",
    )?;
    if original.get("name").and_then(Value::as_str) != Some(package.name())
        || published.get("name") != original.get("name")
    {
        return Err(SdkManifestError::Contract("package.name"));
    }
    if inherited(original, workspace, "edition")? != "2021" {
        return Err(SdkManifestError::Contract("supported package edition"));
    }
    for field in [
        "version",
        "edition",
        "rust-version",
        "license",
        "repository",
        "description",
    ] {
        if published.get(field).and_then(Value::as_str)
            != Some(inherited(original, workspace, field)?)
        {
            return Err(SdkManifestError::Contract(field));
        }
    }
    if original.get("readme").and_then(Value::as_str) != Some("../../docs/rust-sdk.md")
        || published.get("readme").and_then(Value::as_str) != Some("rust-sdk.md")
        || original.get("exclude") != published.get("exclude")
    {
        return Err(SdkManifestError::Contract("package files"));
    }
    for field in [
        "build",
        "autolib",
        "autobins",
        "autoexamples",
        "autotests",
        "autobenches",
    ] {
        if published.get(field).and_then(Value::as_bool) != Some(false) {
            return Err(SdkManifestError::Contract(field));
        }
    }
    Ok(())
}

fn features(value: Option<&Value>) -> Result<BTreeSet<String>, SdkManifestError> {
    let mut result = BTreeSet::new();
    if let Some(value) = value {
        for feature in value
            .as_array()
            .ok_or(SdkManifestError::Contract("dependency features"))?
        {
            result.insert(
                feature
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .ok_or(SdkManifestError::Contract("dependency feature"))?
                    .to_owned(),
            );
        }
    }
    Ok(result)
}

fn dependency_table(value: &Value) -> Result<Table, SdkManifestError> {
    if let Some(version) = value.as_str() {
        Ok(Table::from_iter([(
            "version".to_owned(),
            Value::String(version.to_owned()),
        )]))
    } else {
        value
            .as_table()
            .cloned()
            .ok_or(SdkManifestError::Contract("dependency"))
    }
}

fn resolve_dependency(
    value: &Value,
    name: &str,
    workspace: &Value,
) -> Result<Table, SdkManifestError> {
    let local = dependency_table(value)?;
    if !local.contains_key("workspace") {
        return Ok(local);
    }
    if local.get("workspace").and_then(Value::as_bool) != Some(true) {
        return Err(SdkManifestError::Contract("dependency inheritance"));
    }
    keys(
        &local,
        &["workspace", "features", "optional", "default-features"],
        "dependency inheritance",
    )?;
    let inherited = workspace
        .get("workspace")
        .and_then(|value| value.get("dependencies"))
        .and_then(|value| value.get(name))
        .ok_or(SdkManifestError::Contract("workspace dependency"))?;
    let mut resolved = dependency_table(inherited)?;
    let mut merged = features(resolved.get("features"))?;
    merged.extend(features(local.get("features"))?);
    if !merged.is_empty() {
        resolved.insert(
            "features".to_owned(),
            Value::Array(merged.into_iter().map(Value::String).collect()),
        );
    }
    for field in ["optional", "default-features"] {
        if let Some(value) = local.get(field) {
            if field == "default-features"
                && value.as_bool() == Some(false)
                && resolved.get(field).and_then(Value::as_bool) != Some(false)
            {
                continue;
            }
            resolved.insert(field.to_owned(), value.clone());
        }
    }
    Ok(resolved)
}

fn normalize_dependency(mut value: Table) -> Result<Value, SdkManifestError> {
    keys(
        &value,
        &[
            "version",
            "features",
            "default-features",
            "optional",
            "package",
        ],
        "dependency source",
    )?;
    let version = value
        .get("version")
        .and_then(Value::as_str)
        .ok_or(SdkManifestError::Contract("dependency version"))?;
    let requirement = semver::VersionReq::parse(version)
        .map_err(|_| SdkManifestError::Contract("dependency version"))?;
    if requirement == semver::VersionReq::STAR {
        return Err(SdkManifestError::Contract("dependency version"));
    }
    if let Some(package) = value.get("package") {
        let name = package
            .as_str()
            .ok_or(SdkManifestError::Contract("dependency package"))?;
        public_dependency(name)?;
    }
    let selected = features(value.get("features"))?;
    if selected.is_empty() {
        value.remove("features");
    } else {
        value.insert(
            "features".to_owned(),
            Value::Array(selected.into_iter().map(Value::String).collect()),
        );
    }
    for (field, default) in [("optional", false), ("default-features", true)] {
        if let Some(setting) = value.get(field) {
            let setting = setting
                .as_bool()
                .ok_or(SdkManifestError::Contract("dependency flags"))?;
            if setting == default {
                value.remove(field);
            }
        }
    }
    Ok(Value::Table(value))
}

fn public_dependency(name: &str) -> Result<(), SdkManifestError> {
    if name.starts_with("rusterisk-") && !["rusterisk-api", "rusterisk-core"].contains(&name) {
        Err(SdkManifestError::Contract("backend dependency"))
    } else {
        Ok(())
    }
}

fn dependencies(
    package: SdkPackage,
    original: &Value,
    workspace: &Value,
    published: &Value,
    section: &'static str,
) -> Result<(), SdkManifestError> {
    let mut expected = Table::new();
    if let Some(original) = original.get(section) {
        for (name, value) in table(Some(original), "source dependencies")? {
            let mut resolved = resolve_dependency(value, name, workspace)?;
            if package == SdkPackage::Client && section == "dev-dependencies" {
                let backend_path = match name.as_str() {
                    "rusterisk-server" => Some("../server"),
                    "rusterisk-config" => Some("../config"),
                    "rusterisk-media" => Some("../media"),
                    _ => None,
                };
                if let Some(path) = backend_path {
                    if resolved.len() == 1
                        && resolved.get("path").and_then(Value::as_str) == Some(path)
                    {
                        continue;
                    }
                    return Err(SdkManifestError::Contract("workspace-only dev dependency"));
                }
            }
            public_dependency(name)?;
            if let Some(path) = resolved.remove("path") {
                if path.as_str().is_none() {
                    return Err(SdkManifestError::Contract("source dependency path"));
                }
            }
            expected.insert(name.clone(), normalize_dependency(resolved)?);
        }
    }
    let mut actual = Table::new();
    if let Some(published) = published.get(section) {
        for (name, value) in table(Some(published), "published dependencies")? {
            public_dependency(name)?;
            actual.insert(
                name.clone(),
                normalize_dependency(dependency_table(value)?)?,
            );
        }
    }
    if expected == actual {
        Ok(())
    } else {
        Err(SdkManifestError::Contract(section))
    }
}

fn normalize_lints(value: &Value) -> Result<Value, SdkManifestError> {
    let mut normalized = table(Some(value), "lints")?.clone();
    for (_, group) in normalized.iter_mut() {
        let group = group
            .as_table_mut()
            .ok_or(SdkManifestError::Contract("lint group"))?;
        for (_, lint) in group.iter_mut() {
            if let Some(setting) = lint.as_table_mut() {
                if setting.get("priority").and_then(Value::as_integer) == Some(0) {
                    setting.remove("priority");
                }
                if setting.len() == 1 {
                    if let Some(level) = setting.get("level").and_then(Value::as_str) {
                        *lint = Value::String(level.to_owned());
                    }
                }
            }
        }
    }
    Ok(Value::Table(normalized))
}

fn lints(original: &Value, workspace: &Value, published: &Value) -> Result<(), SdkManifestError> {
    let original = original
        .get("lints")
        .ok_or(SdkManifestError::Contract("source lints"))?;
    let expected = if original.get("workspace").and_then(Value::as_bool) == Some(true) {
        if table(Some(original), "source lints")?.len() != 1 {
            return Err(SdkManifestError::Contract("source lints"));
        }
        workspace
            .get("workspace")
            .and_then(|value| value.get("lints"))
    } else {
        Some(original)
    };
    let expected = expected.ok_or(SdkManifestError::Contract("workspace lints"))?;
    let actual = published
        .get("lints")
        .ok_or(SdkManifestError::Contract("lints"))?;
    if normalize_lints(expected)? != normalize_lints(actual)? {
        return Err(SdkManifestError::Contract("lints"));
    }
    Ok(())
}

fn targets(package: SdkPackage, published: &Value) -> Result<(), SdkManifestError> {
    let library = table(published.get("lib"), "lib")?;
    keys(library, &["name", "path"], "lib fields")?;
    if library.get("path").and_then(Value::as_str) != Some("src/lib.rs")
        || library.get("name").and_then(Value::as_str)
            != Some(package.name().replace('-', "_").as_str())
    {
        return Err(SdkManifestError::Contract("lib"));
    }
    if let Some(tests) = published.get("test") {
        if package == SdkPackage::Client {
            return Err(SdkManifestError::Contract("workspace-only tests"));
        }
        let mut names = BTreeSet::new();
        for test in tests
            .as_array()
            .ok_or(SdkManifestError::Contract("tests"))?
        {
            let test = table(Some(test), "test")?;
            keys(test, &["name", "path"], "test fields")?;
            let name = test
                .get("name")
                .and_then(Value::as_str)
                .ok_or(SdkManifestError::Contract("test name"))?;
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || !names.insert(name)
                || test.get("path").and_then(Value::as_str)
                    != Some(format!("tests/{name}.rs").as_str())
            {
                return Err(SdkManifestError::Contract("test path"));
            }
        }
    }
    Ok(())
}

pub fn check_sdk_manifest(
    package: SdkPackage,
    original: &str,
    workspace: &str,
    published: &str,
) -> Result<(), SdkManifestError> {
    let original = parse(original)?;
    let workspace = parse(workspace)?;
    let published = parse(published)?;
    keys(
        table(Some(&published), "manifest")?,
        &[
            "package",
            "lib",
            "test",
            "dependencies",
            "dev-dependencies",
            "build-dependencies",
            "features",
            "lints",
        ],
        "manifest fields",
    )?;
    metadata(package, &original, &workspace, &published)?;
    targets(package, &published)?;
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        dependencies(package, &original, &workspace, &published, section)?;
    }
    if original.get("features") != published.get("features") {
        return Err(SdkManifestError::Contract("features"));
    }
    lints(&original, &workspace, &published)
}
