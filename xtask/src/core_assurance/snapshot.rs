use super::{normalize_manifest, AssuranceError};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Component, Path, PathBuf},
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

struct Tests<'inputs> {
    files: &'inputs [InputFile],
    names: BTreeSet<String>,
    active: Vec<&'inputs Path>,
}

fn explicit_module_path(module: &syn::ItemMod) -> Result<Option<PathBuf>, AssuranceError> {
    let mut attributes = module
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("path"));
    let Some(attribute) = attributes.next() else {
        return Ok(None);
    };
    let syn::Meta::NameValue(value) = &attribute.meta else {
        return Err(AssuranceError::Manifest);
    };
    let syn::Expr::Lit(expression) = &value.value else {
        return Err(AssuranceError::Manifest);
    };
    let syn::Lit::Str(path) = &expression.lit else {
        return Err(AssuranceError::Manifest);
    };
    let value = path.value();
    let path = Path::new(&value);
    if attributes.next().is_some()
        || value.is_empty()
        || value
            .chars()
            .any(|character| character.is_control() || character == '\\')
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(AssuranceError::Manifest);
    }
    Ok(Some(path.to_owned()))
}

impl<'inputs> Tests<'inputs> {
    fn read(
        files: &'inputs [InputFile],
        root: &'inputs InputFile,
    ) -> Result<BTreeSet<String>, AssuranceError> {
        let mut tests = Self {
            files,
            names: BTreeSet::new(),
            active: Vec::new(),
        };
        tests.file(root, "", None)?;
        Ok(tests.names)
    }

    fn file(
        &mut self,
        file: &'inputs InputFile,
        prefix: &str,
        relative: Option<&str>,
    ) -> Result<(), AssuranceError> {
        if self.active.contains(&file.path.as_path()) {
            return Err(AssuranceError::Manifest);
        }
        let directory = file.path.parent().ok_or(AssuranceError::Manifest)?;
        let inferred = relative.map_or_else(|| directory.to_owned(), |name| directory.join(name));
        let syntax = syn::parse_file(
            std::str::from_utf8(&file.bytes).map_err(|_| AssuranceError::Manifest)?,
        )
        .map_err(AssuranceError::Syntax)?;
        self.active.push(&file.path);
        let result = self.items(&syntax.items, prefix, directory, &inferred);
        self.active.pop().ok_or(AssuranceError::Manifest)?;
        result
    }

    fn items(
        &mut self,
        items: &[syn::Item],
        prefix: &str,
        directory: &Path,
        inferred: &Path,
    ) -> Result<(), AssuranceError> {
        for item in items {
            if let syn::Item::Mod(module) = item {
                self.module(module, prefix, directory, inferred)?;
            }
            let syn::Item::Fn(function) = item else {
                continue;
            };
            if !function
                .attrs
                .iter()
                .any(|attribute| attribute.path().is_ident("test"))
            {
                continue;
            }
            if function
                .attrs
                .iter()
                .any(|attribute| attribute.path().is_ident("ignore"))
                || !self.names.insert(format!("{prefix}{}", function.sig.ident))
            {
                return Err(AssuranceError::Manifest);
            }
        }
        Ok(())
    }

    fn module(
        &mut self,
        module: &syn::ItemMod,
        prefix: &str,
        directory: &Path,
        inferred: &Path,
    ) -> Result<(), AssuranceError> {
        let name = module.ident.to_string();
        let prefix = format!("{prefix}{name}::");
        let explicit = explicit_module_path(module)?.map(|path| directory.join(path));
        if let Some((_, items)) = &module.content {
            let directory = explicit.unwrap_or_else(|| inferred.join(&name));
            return self.items(items, &prefix, &directory, &directory);
        }
        let files = self.files;
        let lookup = |path: &Path| files.iter().find(|file| file.path == path);
        if let Some(explicit) = explicit {
            return self.file(
                lookup(&explicit).ok_or(AssuranceError::Manifest)?,
                &prefix,
                None,
            );
        }
        let flat = inferred.join(format!("{name}.rs"));
        let nested = inferred.join(&name).join("mod.rs");
        match (lookup(&flat), lookup(&nested)) {
            (Some(file), None) => self.file(file, &prefix, Some(&name)),
            (None, Some(file)) => self.file(file, &prefix, None),
            _ => Err(AssuranceError::Manifest),
        }
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
            let found = Tests::read(&files, file)?;
            if found.is_empty() || found.iter().any(|name| tests.contains(name)) {
                return Err(AssuranceError::Manifest);
            }
            tests.extend(found);
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
    use super::{AssuranceError, InputFile, Snapshot, Tests};
    use crate::core_assurance::validate_miri_discovery;
    use std::{collections::BTreeSet, path::Path};

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

    fn omission_is_rejected(path: &str) -> Result<(), AssuranceError> {
        let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/core");
        let snapshot = Snapshot::read(&core)?;
        let transfer = snapshot
            .files
            .iter()
            .find(|file| file.path == Path::new(path))
            .ok_or(AssuranceError::Manifest)?;
        let selected = Tests::read(&snapshot.files, transfer)?;
        assert!(!selected.is_empty());
        assert!(selected.is_subset(&snapshot.tests));
        validate_miri_discovery(&discovery(&snapshot.tests)?, &snapshot.tests, 1)?;
        let missing = snapshot.tests.difference(&selected).cloned().collect();
        assert!(validate_miri_discovery(&discovery(&missing)?, &snapshot.tests, 1).is_err());
        Ok(())
    }

    #[test]
    fn miri_discovery_cannot_omit_transfer_tests_from_the_actual_source(
    ) -> Result<(), AssuranceError> {
        omission_is_rejected("tests/transfer.rs")
    }

    #[test]
    fn miri_discovery_cannot_omit_attended_tests_from_the_actual_source(
    ) -> Result<(), AssuranceError> {
        omission_is_rejected("tests/attended_transfer.rs")
    }

    #[test]
    fn miri_discovery_cannot_omit_owned_pair_tests_from_the_actual_source(
    ) -> Result<(), AssuranceError> {
        omission_is_rejected("tests/owned_call_pair.rs")
    }

    #[test]
    fn miri_inventory_cannot_omit_tests_in_a_file_backed_module() -> Result<(), AssuranceError> {
        let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/core");
        let snapshot = Snapshot::read(&core)?;
        let missing = snapshot
            .tests
            .iter()
            .filter(|name| !name.starts_with("early::"))
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(snapshot.tests.len() - missing.len(), 4);
        assert!(snapshot.targets.contains("music_gate"));
        assert!(validate_miri_discovery(&discovery(&missing)?, &snapshot.tests, 1).is_err());
        Ok(())
    }

    #[test]
    fn native_inventory_follows_declared_modules_without_counting_unreferenced_files(
    ) -> Result<(), AssuranceError> {
        let files = [
            ("tests/example.rs", "mod child; mod inline { mod other; }"),
            ("tests/child.rs", "mod nested; #[test] fn first() {}"),
            ("tests/child/nested.rs", "#[test] fn second() {}"),
            ("tests/inline/other/mod.rs", "#[test] fn third() {}"),
            ("tests/unused.rs", "#[test] fn unused() {}"),
        ]
        .map(|(path, source)| InputFile {
            path: path.into(),
            bytes: source.as_bytes().to_vec(),
        });
        assert_eq!(
            Tests::read(&files, &files[0])?,
            BTreeSet::from([
                "child::first".into(),
                "child::nested::second".into(),
                "inline::other::third".into(),
            ])
        );
        Ok(())
    }

    #[test]
    fn native_inventory_rejects_missing_cyclic_and_ignored_module_inputs() {
        for (declaration, child) in [
            ("mod missing;", "#[test] fn found() {}"),
            ("mod nested;", "#[path = \"nested.rs\"] mod cycle;"),
            ("mod nested;", "#[ignore] #[test] fn found() {}"),
            (
                "#[path = \"../nested.rs\"] mod nested;",
                "#[test] fn found() {}",
            ),
        ] {
            let files = [
                InputFile {
                    path: "tests/example.rs".into(),
                    bytes: declaration.as_bytes().to_vec(),
                },
                InputFile {
                    path: "tests/nested.rs".into(),
                    bytes: child.as_bytes().to_vec(),
                },
            ];
            assert!(Tests::read(&files, &files[0]).is_err());
        }
    }
}
