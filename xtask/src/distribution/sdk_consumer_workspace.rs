use super::{SdkConsumerPlan, SdkConsumerScope, SdkContents, SdkPackage, SdkRuntimeIdentity};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    num::NonZeroUsize,
    path::{Component, Path, PathBuf},
};

const CONTRACT: &[u8] = include_bytes!("consumer_sources/core_contract.rs");
const CORE: &[u8] = include_bytes!("consumer_sources/core_main.rs");
const SDK: &[u8] = include_bytes!("consumer_sources/sdk_main.rs");
const HTTP: &[u8] = include_bytes!("consumer_sources/http_main.rs");
const HTTP_CONTRACT: &[u8] = include_bytes!("consumer_sources/http_contract.rs");

struct Generated {
    path: PathBuf,
    bytes: Vec<u8>,
}

pub struct SdkConsumerWorkspace<'a> {
    root: Option<PathBuf>,
    plan: SdkConsumerPlan,
    contents: &'a SdkContents,
    generated: Vec<Generated>,
    runtime: Option<SdkRuntimeIdentity>,
}

fn invalid(message: &str) -> std::io::Error {
    std::io::Error::other(message)
}

fn independent(root: &Path) -> Result<(), std::io::Error> {
    if !root.is_absolute()
        || root.file_name().is_none()
        || root
            .to_str()
            .is_none_or(|value| value.len() > 4096 || value.chars().any(char::is_control))
        || root
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(invalid("consumer roots require independent absolute paths"));
    }
    let parent = root
        .parent()
        .ok_or_else(|| invalid("consumer parent missing"))?;
    if std::fs::canonicalize(parent)? != parent {
        return Err(invalid("consumer parent must use its physical path"));
    }
    for ancestor in parent.ancestors() {
        for name in [
            ".git",
            "Cargo.toml",
            "mise.toml",
            ".mise.toml",
            ".cargo/config.toml",
            ".cargo/config",
        ] {
            match std::fs::symlink_metadata(ancestor.join(name)) {
                Ok(_) => {
                    return Err(invalid(
                        "consumer roots cannot inherit project configuration",
                    ))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

fn consumer_manifest(
    scope: SdkConsumerScope,
    version: &str,
    runtime: Option<&SdkRuntimeIdentity>,
) -> Result<Vec<u8>, std::io::Error> {
    let selected: &[SdkPackage] = match scope {
        SdkConsumerScope::CoreMsrv => &[SdkPackage::Core],
        SdkConsumerScope::Sdk => &SdkPackage::ALL,
    };
    let mut package = toml::Table::from_iter([
        (
            "name".to_owned(),
            toml::Value::String(scope.package_name().to_owned()),
        ),
        (
            "version".to_owned(),
            toml::Value::String("0.0.0".to_owned()),
        ),
        ("edition".to_owned(), toml::Value::String("2021".to_owned())),
        ("publish".to_owned(), toml::Value::Boolean(false)),
    ]);
    for field in [
        "build",
        "autolib",
        "autobins",
        "autoexamples",
        "autotests",
        "autobenches",
    ] {
        package.insert(field.to_owned(), toml::Value::Boolean(false));
    }
    let mut dependencies: toml::Table = selected
        .iter()
        .map(|package| {
            (
                package.name().to_owned(),
                toml::Value::String(format!("={version}")),
            )
        })
        .collect();
    if let (SdkConsumerScope::Sdk, Some(runtime)) = (scope, runtime) {
        dependencies.insert(
            "tokio".to_owned(),
            toml::Value::Table(toml::Table::from_iter([
                (
                    "version".to_owned(),
                    toml::Value::String(format!("={}", runtime.version())),
                ),
                ("default-features".to_owned(), toml::Value::Boolean(false)),
                (
                    "features".to_owned(),
                    toml::Value::Array(
                        ["net", "rt", "time"]
                            .into_iter()
                            .map(|feature| toml::Value::String(feature.to_owned()))
                            .collect(),
                    ),
                ),
            ])),
        );
    }
    let overrides: toml::Table = selected
        .iter()
        .map(|package| {
            (
                package.name().to_owned(),
                toml::Value::Table(toml::Table::from_iter([(
                    "path".to_owned(),
                    toml::Value::String(format!("../../packages/{}-{version}", package.name())),
                )])),
            )
        })
        .collect();
    let binary = toml::Value::Table(toml::Table::from_iter([
        (
            "name".to_owned(),
            toml::Value::String(scope.package_name().to_owned()),
        ),
        (
            "path".to_owned(),
            toml::Value::String("src/main.rs".to_owned()),
        ),
    ]));
    let manifest = toml::Table::from_iter([
        ("package".to_owned(), toml::Value::Table(package)),
        (
            "workspace".to_owned(),
            toml::Value::Table(toml::Table::from_iter([(
                "resolver".to_owned(),
                toml::Value::String("2".to_owned()),
            )])),
        ),
        ("dependencies".to_owned(), toml::Value::Table(dependencies)),
        (
            "patch".to_owned(),
            toml::Value::Table(toml::Table::from_iter([(
                "crates-io".to_owned(),
                toml::Value::Table(overrides),
            )])),
        ),
        (
            "bin".to_owned(),
            toml::Value::Array(std::iter::once(binary).collect()),
        ),
    ]);
    toml::to_string(&manifest)
        .map(String::into_bytes)
        .map_err(std::io::Error::other)
}

fn generated(
    contents: &SdkContents,
    runtime: Option<&SdkRuntimeIdentity>,
) -> Result<Vec<Generated>, std::io::Error> {
    let mut files = Vec::new();
    for scope in SdkConsumerScope::ALL {
        let directory = Path::new("consumers").join(scope.label());
        files.push(Generated {
            path: directory.join("Cargo.toml"),
            bytes: consumer_manifest(scope, contents.version(), runtime)?,
        });
        files.push(Generated {
            path: directory.join("src/core_contract.rs"),
            bytes: CONTRACT.to_vec(),
        });
        files.push(Generated {
            path: directory.join("src/main.rs"),
            bytes: match scope {
                SdkConsumerScope::CoreMsrv => CORE,
                SdkConsumerScope::Sdk if runtime.is_some() => HTTP,
                SdkConsumerScope::Sdk => SDK,
            }
            .to_vec(),
        });
        if scope == SdkConsumerScope::Sdk && runtime.is_some() {
            files.push(Generated {
                path: directory.join("src/http_contract.rs"),
                bytes: HTTP_CONTRACT.to_vec(),
            });
        }
        let package = match scope {
            SdkConsumerScope::CoreMsrv => SdkPackage::Core,
            SdkConsumerScope::Sdk => SdkPackage::Client,
        };
        let seed = contents
            .archive(package)
            .files()
            .iter()
            .find(|file| file.path().as_str() == "Cargo.lock")
            .ok_or_else(|| invalid("candidate seed lockfile missing"))?;
        files.push(Generated {
            path: directory.join("Cargo.lock"),
            bytes: seed.bytes().to_vec(),
        });
    }
    Ok(files)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)
}

impl<'a> SdkConsumerWorkspace<'a> {
    pub fn create(contents: &'a SdkContents, root: &Path) -> Result<Self, std::io::Error> {
        Self::create_with(contents, root, write_new)
    }

    pub fn create_with_jobs(
        contents: &'a SdkContents,
        root: &Path,
        jobs: NonZeroUsize,
    ) -> Result<Self, std::io::Error> {
        Self::create_profile(contents, root, None, jobs, write_new)
    }

    pub fn create_with(
        contents: &'a SdkContents,
        root: &Path,
        write: impl FnMut(&Path, &[u8]) -> Result<(), std::io::Error>,
    ) -> Result<Self, std::io::Error> {
        Self::create_profile(contents, root, None, SdkConsumerPlan::DEFAULT_JOBS, write)
    }

    pub fn create_http(contents: &'a SdkContents, root: &Path) -> Result<Self, std::io::Error> {
        Self::create_http_with_jobs(contents, root, SdkConsumerPlan::DEFAULT_JOBS)
    }

    pub fn create_http_with_jobs(
        contents: &'a SdkContents,
        root: &Path,
        jobs: NonZeroUsize,
    ) -> Result<Self, std::io::Error> {
        let runtime = SdkRuntimeIdentity::from_contents(contents).map_err(std::io::Error::other)?;
        Self::create_profile(contents, root, Some(runtime), jobs, write_new)
    }

    fn create_profile(
        contents: &'a SdkContents,
        root: &Path,
        runtime: Option<SdkRuntimeIdentity>,
        jobs: NonZeroUsize,
        mut write: impl FnMut(&Path, &[u8]) -> Result<(), std::io::Error>,
    ) -> Result<Self, std::io::Error> {
        independent(root)?;
        let plan = SdkConsumerPlan::new_with_jobs(&root.join("work"), &root.join("state"), jobs)?;
        let generated = generated(contents, runtime.as_ref())?;
        std::fs::create_dir(root)?;
        let workspace = Self {
            root: Some(root.to_owned()),
            plan,
            contents,
            generated,
            runtime,
        };
        let operation = (|| {
            std::fs::create_dir(workspace.plan.work())?;
            std::fs::create_dir(workspace.plan.state())?;
            for (path, bytes) in workspace.expected() {
                let path = workspace.plan.work().join(path);
                std::fs::create_dir_all(
                    path.parent()
                        .ok_or_else(|| invalid("consumer file parent missing"))?,
                )?;
                write(&path, bytes)?;
            }
            workspace.check_initial_locks()?;
            workspace.check_sources()
        })();
        match operation {
            Ok(()) => Ok(workspace),
            Err(operation) => match workspace.close() {
                Ok(()) => Err(operation),
                Err(cleanup) => Err(std::io::Error::other(format!(
                    "{operation}; consumer setup cleanup failed: {cleanup}"
                ))),
            },
        }
    }

    pub fn plan(&self) -> &SdkConsumerPlan {
        &self.plan
    }

    pub fn runtime(&self) -> Option<&SdkRuntimeIdentity> {
        self.runtime.as_ref()
    }

    fn check_initial_locks(&self) -> Result<(), std::io::Error> {
        for scope in SdkConsumerScope::ALL {
            let relative = Path::new("consumers")
                .join(scope.label())
                .join("Cargo.lock");
            let expected = self
                .generated
                .iter()
                .find(|file| file.path == relative)
                .ok_or_else(|| invalid("consumer seed lock missing"))?;
            let mut bytes = Vec::new();
            std::fs::File::open(self.plan.work().join(relative))?
                .take(expected.bytes.len() as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes != expected.bytes {
                return Err(invalid("consumer setup changed candidate seed lock bytes"));
            }
        }
        Ok(())
    }

    fn expected(&self) -> BTreeMap<PathBuf, &[u8]> {
        let mut files = BTreeMap::new();
        for package in SdkPackage::ALL {
            let directory = Path::new("packages").join(format!(
                "{}-{}",
                package.name(),
                self.contents.version()
            ));
            for file in self.contents.archive(package).files() {
                files.insert(directory.join(file.path().as_str()), file.bytes());
            }
        }
        files.extend(
            self.generated
                .iter()
                .map(|file| (file.path.clone(), file.bytes.as_slice())),
        );
        files
    }

    pub fn check_sources(&self) -> Result<(), std::io::Error> {
        let root = self
            .root
            .as_deref()
            .ok_or_else(|| invalid("consumer root ownership ended"))?;
        if !std::fs::symlink_metadata(root)?.file_type().is_dir() {
            return Err(invalid("consumer root changed"));
        }
        let mut directories = 0;
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            if !["work", "state"]
                .iter()
                .any(|name| entry.file_name() == *name)
                || !entry.file_type()?.is_dir()
            {
                return Err(invalid("consumer root contains unexpected configuration"));
            }
            directories += 1;
        }
        if directories != 2 {
            return Err(invalid("consumer work or state directory is missing"));
        }
        let expected = self.expected();
        let mutable = SdkConsumerScope::ALL.map(|scope| {
            Path::new("consumers")
                .join(scope.label())
                .join("Cargo.lock")
        });
        let mut check = Check {
            root: self.plan.work(),
            expected,
            mutable,
            seen: BTreeSet::new(),
            entries: 0,
        };
        check.walk(Path::new(""), 0)?;
        if check.seen.len() != check.expected.len() {
            return Err(invalid("consumer working copy is missing captured source"));
        }
        Ok(())
    }

    pub fn close(mut self) -> Result<(), std::io::Error> {
        let root = self
            .root
            .take()
            .ok_or_else(|| invalid("consumer root ownership ended"))?;
        std::fs::remove_dir_all(root)
    }
}

impl Drop for SdkConsumerWorkspace<'_> {
    fn drop(&mut self) {
        if let Some(root) = self.root.take() {
            if let Err(error) = std::fs::remove_dir_all(root) {
                eprintln!("consumer fallback cleanup failed: {error}");
            }
        }
    }
}

struct Check<'a> {
    root: &'a Path,
    expected: BTreeMap<PathBuf, &'a [u8]>,
    mutable: [PathBuf; 2],
    seen: BTreeSet<PathBuf>,
    entries: usize,
}

impl Check<'_> {
    fn walk(&mut self, relative: &Path, depth: usize) -> Result<(), std::io::Error> {
        if depth > 20 {
            return Err(invalid("consumer directory depth exceeded"));
        }
        let directory = self.root.join(relative);
        if !std::fs::symlink_metadata(&directory)?.file_type().is_dir() {
            return Err(invalid("consumer source directory changed"));
        }
        for entry in std::fs::read_dir(directory)? {
            self.entries += 1;
            if self.entries > 16384 {
                return Err(invalid("consumer source entry count exceeded"));
            }
            let entry = entry?;
            let relative = relative.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if !self.expected.keys().any(|path| path.starts_with(&relative)) {
                    return Err(invalid("unexpected consumer source directory"));
                }
                self.walk(&relative, depth + 1)?;
            } else if kind.is_file() {
                let expected = self
                    .expected
                    .get(&relative)
                    .copied()
                    .ok_or_else(|| invalid("unexpected consumer source file"))?;
                let mutable = self.mutable.contains(&relative);
                let limit = if mutable {
                    2 * 1024 * 1024
                } else {
                    expected.len()
                };
                let mut bytes = Vec::new();
                std::fs::File::open(entry.path())?
                    .take(limit as u64 + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > limit || (!mutable && bytes != expected) {
                    return Err(invalid("consumer working copy changed captured bytes"));
                }
                self.seen.insert(relative);
            } else {
                return Err(invalid(
                    "consumer source must remain regular without symlinks",
                ));
            }
        }
        Ok(())
    }
}
