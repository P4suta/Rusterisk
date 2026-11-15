use super::AssuranceError;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    package: Package,
    lib: Library,
    test: Vec<Target>,
    workspace: BTreeMap<String, serde::de::IgnoredAny>,
    lints: BTreeMap<String, serde::de::IgnoredAny>,
    #[serde(default)]
    dependencies: BTreeMap<String, serde::de::IgnoredAny>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Package {
    name: String,
    version: String,
    edition: String,
    #[serde(rename = "rust-version")]
    rust_version: String,
    license: String,
    publish: bool,
    autotests: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Library {
    name: String,
    path: String,
    test: bool,
    doctest: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    name: String,
    path: String,
}

pub fn normalize_manifest(
    source: &str,
    expected: &BTreeSet<String>,
) -> Result<String, AssuranceError> {
    let manifest: Manifest = toml::from_str(source).map_err(|_| AssuranceError::Manifest)?;
    let package = manifest.package;
    if package.name != "rusterisk-core-assurance"
        || package.version != "0.1.0"
        || package.edition != "2021"
        || package.rust_version != "1.89"
        || package.license != "MIT OR Apache-2.0"
        || package.publish
        || package.autotests
        || manifest.lib.name != "rusterisk_core"
        || manifest.lib.path != "../src/lib.rs"
        || manifest.lib.test
        || manifest.lib.doctest
        || !manifest.workspace.is_empty()
        || manifest.lints.is_empty()
        || !manifest.dependencies.is_empty()
        || expected.is_empty()
        || expected.len() > 64
    {
        return Err(AssuranceError::Manifest);
    }
    let mut names = BTreeSet::new();
    for target in manifest.test {
        if !expected.contains(&target.name)
            || target.path != format!("../tests/{}.rs", target.name)
            || !names.insert(target.name)
        {
            return Err(AssuranceError::Manifest);
        }
    }
    if &names != expected {
        return Err(AssuranceError::Manifest);
    }
    let mut table: toml::Table = toml::from_str(source).map_err(|_| AssuranceError::Manifest)?;
    let lib = table
        .get_mut("lib")
        .and_then(toml::Value::as_table_mut)
        .ok_or(AssuranceError::Manifest)?;
    lib.insert("path".into(), toml::Value::String("src/lib.rs".into()));
    for target in table
        .get_mut("test")
        .and_then(toml::Value::as_array_mut)
        .ok_or(AssuranceError::Manifest)?
    {
        let target = target.as_table_mut().ok_or(AssuranceError::Manifest)?;
        let name = target
            .get("name")
            .and_then(toml::Value::as_str)
            .ok_or(AssuranceError::Manifest)?;
        let path = format!("tests/{name}.rs");
        target.insert("path".into(), toml::Value::String(path));
    }
    toml::to_string(&table).map_err(|_| AssuranceError::Manifest)
}
