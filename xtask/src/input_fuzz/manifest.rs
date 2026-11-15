use super::{FuzzError, Target};

pub fn normalize_manifest(source: &str) -> Result<String, FuzzError> {
    let mut manifest: toml::Value = toml::from_str(source).map_err(FuzzError::Toml)?;
    let package = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .ok_or(FuzzError::Manifest)?;
    if package
        .get("metadata")
        .and_then(|metadata| metadata.get("cargo-fuzz"))
        .and_then(toml::Value::as_bool)
        != Some(true)
        || package.get("rust-version").and_then(toml::Value::as_str) != Some("1.99")
        || package.get("autobins").and_then(toml::Value::as_bool) != Some(false)
        || manifest
            .get("workspace")
            .and_then(toml::Value::as_table)
            .is_none()
    {
        return Err(FuzzError::Manifest);
    }
    let bins = manifest
        .get("bin")
        .and_then(toml::Value::as_array)
        .ok_or(FuzzError::Manifest)?;
    if bins.len() != Target::ALL.len() {
        return Err(FuzzError::Manifest);
    }
    for target in Target::ALL {
        let matching: Vec<_> = bins
            .iter()
            .filter(|bin| bin.get("name").and_then(toml::Value::as_str) == Some(target.name()))
            .collect();
        if matching.len() != 1 {
            return Err(FuzzError::Manifest);
        }
        let bin = matching[0];
        let path = format!("fuzz_targets/{}.rs", target.name());
        let required: Vec<_> = target.features().split(',').collect();
        let features = bin
            .get("required-features")
            .and_then(toml::Value::as_array)
            .ok_or(FuzzError::Manifest)?;
        if bin.get("path").and_then(toml::Value::as_str) != Some(path.as_str())
            || ["test", "doc", "bench"]
                .iter()
                .any(|field| bin.get(field).and_then(toml::Value::as_bool) != Some(false))
            || features
                .iter()
                .map(toml::Value::as_str)
                .collect::<Option<Vec<_>>>()
                .as_deref()
                != Some(required.as_slice())
        {
            return Err(FuzzError::Manifest);
        }
    }
    let dependencies = manifest
        .get_mut("dependencies")
        .and_then(toml::Value::as_table_mut)
        .ok_or(FuzzError::Manifest)?;
    if dependencies.len() != 5
        || dependencies
            .get("libfuzzer-sys")
            .and_then(|dependency| dependency.get("version"))
            .and_then(toml::Value::as_str)
            != Some("=0.4.13")
    {
        return Err(FuzzError::Manifest);
    }
    for (name, directory) in [
        ("rusterisk-api", "api"),
        ("rusterisk-config", "config"),
        ("rusterisk-sip", "sip"),
        ("rusterisk-media", "media"),
    ] {
        let dependency = dependencies
            .get_mut(name)
            .and_then(toml::Value::as_table_mut)
            .ok_or(FuzzError::Manifest)?;
        if dependency.get("version").and_then(toml::Value::as_str) != Some("=0.1.0")
            || dependency.get("path").and_then(toml::Value::as_str)
                != Some(format!("../../crates/{directory}").as_str())
        {
            return Err(FuzzError::Manifest);
        }
        dependency.insert(
            "path".into(),
            toml::Value::String(format!("/project/crates/{directory}")),
        );
    }
    toml::to_string(&manifest).map_err(|_| FuzzError::Manifest)
}

pub fn validate_locks(production: &str, fuzz: &str) -> Result<(), FuzzError> {
    let production: toml::Value = toml::from_str(production).map_err(FuzzError::Toml)?;
    let fuzz: toml::Value = toml::from_str(fuzz).map_err(FuzzError::Toml)?;
    let production = production
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or(FuzzError::Manifest)?;
    let fuzz = fuzz
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or(FuzzError::Manifest)?;
    if production.is_empty() || fuzz.is_empty() || fuzz.len() > 2048 {
        return Err(FuzzError::Manifest);
    }
    for dependency in fuzz {
        let name = dependency
            .get("name")
            .and_then(toml::Value::as_str)
            .ok_or(FuzzError::Manifest)?;
        let version = dependency
            .get("version")
            .and_then(toml::Value::as_str)
            .ok_or(FuzzError::Manifest)?;
        if name == "rusterisk-input-fuzz" && version == "0.1.0" {
            continue;
        }
        if name == "libfuzzer-sys"
            && version == "0.4.13"
            && dependency.get("checksum").and_then(toml::Value::as_str)
                == Some("a9fd2f41a1cba099f79a0b6b6c35656cf7c03351a7bae8ff0f28f25270f929d2")
        {
            continue;
        }
        if name == "arbitrary"
            && dependency.get("source").and_then(toml::Value::as_str)
                == Some("registry+https://github.com/rust-lang/crates.io-index")
        {
            continue;
        }
        if !production.iter().any(|package| {
            ["name", "version", "source", "checksum"]
                .iter()
                .all(|field| package.get(field) == dependency.get(field))
        }) {
            return Err(FuzzError::Manifest);
        }
    }
    Ok(())
}
