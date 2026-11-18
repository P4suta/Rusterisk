use super::{
    follow, read, tools, CapturedCoreMiri, CheckedMiri, CommandMode, CommandSupervisor,
    CoreAssurancePlan, CoreMiriError, CoreMiriOutcome, MiriInventory, NativeFiles, WorkingMiri,
    PHASES,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MiriExecution {
    identity: String,
    directory: PathBuf,
}

impl MiriExecution {
    pub(crate) fn check(&self, image: &str) -> Result<(), CoreMiriError> {
        let directory = self.directory.to_str().ok_or(CoreMiriError::Plan)?;
        if !self.directory.is_absolute()
            || directory.len() > 4096
            || directory
                .chars()
                .any(|character| character.is_control() || matches!(character, '\\' | ':' | ','))
            || directory
                .split('/')
                .skip(1)
                .any(|part| part.is_empty() || matches!(part, "." | ".."))
            || self.directory.components().any(|part| {
                matches!(
                    part,
                    Component::Prefix(_) | Component::CurDir | Component::ParentDir
                )
            })
            || self.directory.file_name().and_then(|name| name.to_str())
                != Some(format!("rusterisk-core-miri-{}", self.identity).as_str())
        {
            return Err(CoreMiriError::Plan);
        }
        CoreAssurancePlan::new(
            &self.identity,
            self.directory
                .join("source")
                .to_str()
                .ok_or(CoreMiriError::Plan)?,
            self.directory
                .join("state")
                .to_str()
                .ok_or(CoreMiriError::Plan)?,
            image,
        )?;
        Ok(())
    }
}

fn report_files() -> impl Iterator<Item = String> {
    PHASES
        .into_iter()
        .flat_map(|phase| {
            [
                format!("{}.txt", phase.label()),
                format!("{}.stderr.txt", phase.label()),
            ]
        })
        .chain(tools::files())
}

fn preserve_reports(work: &Path, root: &Path) -> Result<(), CoreMiriError> {
    fs::create_dir(root)?;
    fs::create_dir(root.join("state"))?;
    fs::create_dir(root.join("state/tools"))?;
    for name in report_files() {
        let bytes = match read(&work.join("state").join(&name)) {
            Ok(bytes) => bytes,
            Err(CoreMiriError::Io(error)) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join("state").join(name))?
            .write_all(&bytes)?;
    }
    Ok(())
}

fn expected(capture: &CapturedCoreMiri) -> BTreeSet<PathBuf> {
    capture
        .inputs()
        .map(|(path, _)| Path::new("original").join(path))
        .chain(capture.inputs().filter_map(|(path, _)| {
            let path = path.strip_prefix("crates/core").ok()?;
            (path.starts_with("src") || path.starts_with("tests"))
                .then(|| Path::new("source").join(path))
        }))
        .chain([
            PathBuf::from("source/Cargo.toml"),
            PathBuf::from("source/Cargo.lock"),
        ])
        .chain(report_files().map(|name| Path::new("state").join(name)))
        .collect()
}

fn check_files(
    root: &Path,
    relative: &Path,
    remaining: &mut BTreeSet<PathBuf>,
) -> Result<(), CoreMiriError> {
    if relative.components().count() > 12 || !fs::symlink_metadata(root.join(relative))?.is_dir() {
        return Err(CoreMiriError::Plan);
    }
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let path = relative.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if !remaining.iter().any(|file| file.starts_with(&path)) {
                return Err(CoreMiriError::Plan);
            }
            check_files(root, &path, remaining)?;
        } else if !kind.is_file() || !remaining.remove(&path) {
            return Err(CoreMiriError::Plan);
        }
    }
    Ok(())
}

fn validate(capture: &CapturedCoreMiri, root: &Path) -> Result<MiriInventory, CoreMiriError> {
    let mut remaining = expected(capture);
    check_files(root, Path::new(""), &mut remaining)?;
    if !remaining.is_empty() {
        return Err(CoreMiriError::Plan);
    }
    capture.check(&root.join("original"), &root.join("source"))?;
    let state = root.join("state");
    tools::check(&state)?;
    for phase in PHASES {
        read(&state.join(format!("{}.stderr.txt", phase.label())))?;
    }
    read(&state.join("miri-setup.txt"))?;
    let inventory = crate::core_assurance::validate_miri_discovery(
        &read(&state.join("miri-inventory.txt"))?,
        capture.tests(),
        capture.targets().len(),
    )?;
    crate::core_assurance::validate_miri_execution(
        &read(&state.join("miri-tests.txt"))?,
        &inventory,
    )?;
    Ok(inventory)
}

impl CapturedCoreMiri {
    pub(crate) fn verify_retained(
        self,
        image: &str,
        root: &Path,
    ) -> Result<(CoreMiriOutcome, MiriExecution), CoreMiriError> {
        let mut supervisor = CommandSupervisor::new().map_err(CoreMiriError::Process)?;
        let work = match WorkingMiri::new(self, image, NativeFiles::Retained) {
            Ok(work) => work,
            Err(error) => {
                return follow(
                    Err(error),
                    supervisor.finish().map_err(CoreMiriError::Process),
                    "Miri supervisor cleanup failed",
                )
            }
        };
        let operation = work.execute(&mut |command, budget, mode| match mode {
            CommandMode::Work => supervisor.run(command, budget),
            CommandMode::Cleanup => supervisor.run_cleanup(command, budget),
        });
        let operation = follow(
            operation,
            supervisor.finish().map_err(CoreMiriError::Process),
            "Miri supervisor cleanup failed",
        );
        let operation = follow(operation, work.check(), "Miri final input check failed");
        let operation = follow(
            operation,
            preserve_reports(&work.directory, root),
            "Miri native report preservation failed",
        );
        let operation = operation.and_then(|inventory| {
            work.capture
                .write(&root.join("original"), &root.join("source"))?;
            validate(&work.capture, root)?;
            Ok(inventory)
        });
        let execution = MiriExecution {
            identity: work.identity.clone(),
            directory: work.directory.clone(),
        };
        let CheckedMiri { capture, image, .. } = work.close(operation)?;
        let outcome = capture.read_retained(image.as_str(), root, &execution)?;
        Ok((outcome, execution))
    }

    pub(crate) fn read_retained(
        self,
        image: &str,
        root: &Path,
        execution: &MiriExecution,
    ) -> Result<CoreMiriOutcome, CoreMiriError> {
        execution.check(image)?;
        let image = crate::reference::ImageIdentity::parse(image).ok_or(CoreMiriError::Plan)?;
        let inventory = validate(&self, root)?;
        Ok(CoreMiriOutcome {
            capture: self,
            inventory,
            image,
        })
    }
}
