use super::{normalize_manifest, AssuranceError};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};

#[derive(PartialEq, Eq)]
pub(super) struct InputFile {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

pub(super) struct Snapshot {
    pub files: Vec<InputFile>,
    pub normalized: String,
    pub targets: BTreeSet<String>,
    pub tests: BTreeSet<String>,
}

pub(super) fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, AssuranceError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(AssuranceError::Plan);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(AssuranceError::Plan);
    }
    Ok(bytes)
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
struct Dependencies {
    #[serde(default)]
    dependencies: BTreeMap<String, serde::de::IgnoredAny>,
    #[serde(default)]
    dev_dependencies: BTreeMap<String, serde::de::IgnoredAny>,
    #[serde(default)]
    build_dependencies: BTreeMap<String, serde::de::IgnoredAny>,
    #[serde(default)]
    target: BTreeMap<String, serde::de::IgnoredAny>,
}

#[derive(Default)]
struct Tests {
    names: BTreeSet<String>,
    modules: Vec<String>,
    invalid: bool,
}
impl<'ast> syn::visit::Visit<'ast> for Tests {
    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        self.modules.push(module.ident.to_string());
        syn::visit::visit_item_mod(self, module);
        if self.modules.pop().is_none() {
            self.invalid = true;
        }
    }
    fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
        if function
            .attrs
            .iter()
            .any(|attribute| attribute.path().is_ident("test"))
        {
            let mut name = self.modules.join("::");
            if !name.is_empty() {
                name.push_str("::");
            }
            name.push_str(&function.sig.ident.to_string());
            if function
                .attrs
                .iter()
                .any(|attribute| attribute.path().is_ident("ignore"))
                || !self.names.insert(name)
            {
                self.invalid = true;
            }
        }
        syn::visit::visit_item_fn(self, function);
    }
}

impl Snapshot {
    pub fn read(core: &Path) -> Result<Self, AssuranceError> {
        fn visit(
            core: &Path,
            directory: &Path,
            depth: u8,
            files: &mut Vec<InputFile>,
            total: &mut usize,
        ) -> Result<(), AssuranceError> {
            if depth > 8 {
                return Err(AssuranceError::Plan);
            }
            for entry in std::fs::read_dir(directory)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                let path = entry.path();
                if kind.is_symlink() {
                    return Err(AssuranceError::Plan);
                }
                if kind.is_dir() {
                    visit(core, &path, depth + 1, files, total)?;
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    if files.len() >= 128 {
                        return Err(AssuranceError::Plan);
                    }
                    let bytes = read_bounded(&path, 1024 * 1024)?;
                    *total += bytes.len();
                    if *total > 8 * 1024 * 1024 {
                        return Err(AssuranceError::Plan);
                    }
                    files.push(InputFile {
                        path: path
                            .strip_prefix(core)
                            .map_err(|_| AssuranceError::Plan)?
                            .into(),
                        bytes,
                    });
                }
            }
            Ok(())
        }
        let mut files = Vec::new();
        let mut total = 0;
        visit(core, &core.join("src"), 0, &mut files, &mut total)?;
        visit(core, &core.join("tests"), 0, &mut files, &mut total)?;
        for relative in ["Cargo.toml", "assurance/Cargo.toml", "assurance/Cargo.lock"] {
            let bytes = read_bounded(&core.join(relative), 16384)?;
            total += bytes.len();
            if total > 8 * 1024 * 1024 {
                return Err(AssuranceError::Plan);
            }
            files.push(InputFile {
                path: relative.into(),
                bytes,
            });
        }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let source = |path: &str| -> Result<&str, AssuranceError> {
            std::str::from_utf8(
                &files
                    .iter()
                    .find(|file| file.path == Path::new(path))
                    .ok_or(AssuranceError::Manifest)?
                    .bytes,
            )
            .map_err(|_| AssuranceError::Manifest)
        };
        let dependencies: Dependencies =
            toml::from_str(source("Cargo.toml")?).map_err(|_| AssuranceError::Manifest)?;
        if !dependencies.dependencies.is_empty()
            || !dependencies.dev_dependencies.is_empty()
            || !dependencies.build_dependencies.is_empty()
            || !dependencies.target.is_empty()
            || core.join("build.rs").try_exists()?
        {
            return Err(AssuranceError::Manifest);
        }
        let mut targets = BTreeSet::new();
        let mut tests = BTreeSet::new();
        use syn::visit::Visit;
        for file in &files {
            if file.path.parent() != Some(Path::new("tests"))
                || file
                    .path
                    .extension()
                    .is_none_or(|extension| extension != "rs")
            {
                continue;
            }
            let name = file
                .path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or(AssuranceError::Manifest)?
                .to_owned();
            if !targets.insert(name) {
                return Err(AssuranceError::Manifest);
            }
            let parsed = syn::parse_file(
                std::str::from_utf8(&file.bytes).map_err(|_| AssuranceError::Manifest)?,
            )
            .map_err(AssuranceError::Syntax)?;
            let mut found = Tests::default();
            found.visit_file(&parsed);
            if found.invalid
                || found.names.is_empty()
                || found.names.iter().any(|name| tests.contains(name))
            {
                return Err(AssuranceError::Manifest);
            }
            tests.extend(found.names);
        }
        if tests.is_empty() || tests.len() > 1024 {
            return Err(AssuranceError::Inventory);
        }
        let normalized = normalize_manifest(source("assurance/Cargo.toml")?, &targets)?;
        Ok(Self {
            files,
            normalized,
            targets,
            tests,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{AssuranceError, Snapshot, Tests};
    use crate::core_assurance::validate_miri_discovery;
    use std::{collections::BTreeSet, path::Path};
    use syn::visit::Visit;

    fn discovery(names: &BTreeSet<String>) -> Result<Vec<u8>, AssuranceError> {
        let mut source =
            serde_json::to_string(&serde_json::json!({"type":"suite", "event":"discovery"}))
                .map_err(AssuranceError::Json)?;
        source.push('\n');
        for name in names {
            source.push_str(&serde_json::to_string(&serde_json::json!({"type":"test", "event":"discovered", "name":name, "ignore":false})).map_err(AssuranceError::Json)?);
            source.push('\n');
        }
        source.push_str(&serde_json::to_string(&serde_json::json!({"type":"suite", "event":"completed", "tests":names.len(), "benchmarks":0, "total":names.len(), "ignored":0})).map_err(AssuranceError::Json)?);
        source.push('\n');
        Ok(source.into_bytes())
    }

    #[test]
    fn miri_discovery_cannot_omit_transfer_tests_from_the_actual_source(
    ) -> Result<(), AssuranceError> {
        let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/core");
        let snapshot = Snapshot::read(&core)?;
        let transfer = snapshot
            .files
            .iter()
            .find(|file| file.path == Path::new("tests/transfer.rs"))
            .ok_or(AssuranceError::Manifest)?;
        let file = syn::parse_file(
            std::str::from_utf8(&transfer.bytes).map_err(|_| AssuranceError::Manifest)?,
        )
        .map_err(AssuranceError::Syntax)?;
        let mut selected = Tests::default();
        selected.visit_file(&file);
        assert!(!selected.names.is_empty());
        assert!(selected.names.is_subset(&snapshot.tests));
        validate_miri_discovery(&discovery(&snapshot.tests)?, &snapshot.tests, 1)?;
        let missing = snapshot
            .tests
            .difference(&selected.names)
            .cloned()
            .collect();
        assert!(validate_miri_discovery(&discovery(&missing)?, &snapshot.tests, 1).is_err());
        Ok(())
    }
}
