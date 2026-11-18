use super::{
    complete_all, phases, validate_phase, CheckedFuzz, CommandMode, CommandSupervisor,
    FuzzIdentity, ImageIdentity, InputFuzzOutcome, Scope, TargetCompletion, WorkingFuzz,
};
use crate::input_fuzz::{
    snapshot::{read_directory, read_under, write_new},
    CapturedInputFuzz, FuzzError, FuzzPlan, LinuxTarget, Target, MAX_INPUT,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Component, Path, PathBuf},
};

#[cfg(test)]
mod tests;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FuzzExecution {
    identity: String,
    directory: PathBuf,
}

impl FuzzExecution {
    pub(crate) fn check(&self, image: &str, architecture: LinuxTarget) -> Result<(), FuzzError> {
        let directory = self.directory.to_str().ok_or(FuzzError::Plan)?;
        if !self.directory.is_absolute()
            || directory.len() > 4096
            || directory
                .chars()
                .any(|value| value.is_control() || matches!(value, '\\' | ':' | ','))
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
                != Some(format!("rusterisk-input-fuzz-{}", self.identity).as_str())
        {
            return Err(FuzzError::Plan);
        }
        FuzzPlan::new(
            &self.identity,
            self.directory
                .join("source")
                .to_str()
                .ok_or(FuzzError::Plan)?,
            self.directory
                .join("state")
                .to_str()
                .ok_or(FuzzError::Plan)?,
            image,
            architecture,
        )?;
        Ok(())
    }
}

fn report_files() -> Result<impl Iterator<Item = PathBuf>, FuzzError> {
    Ok(phases(Scope::All)?
        .into_iter()
        .map(|phase| Path::new("state").join(format!("{}.txt", phase.label()))))
}

fn corpus_files() -> impl Iterator<Item = (PathBuf, &'static [u8])> {
    Target::ALL.into_iter().flat_map(|target| {
        target.seeds().iter().map(move |seed| {
            (
                Path::new("state/corpus")
                    .join(target.name())
                    .join(seed.name),
                seed.bytes,
            )
        })
    })
}

fn expected(capture: &CapturedInputFuzz) -> Result<BTreeSet<PathBuf>, FuzzError> {
    Ok(capture
        .inputs()
        .map(|(path, _)| Path::new("source").join(path))
        .chain(capture.inputs().filter_map(|(path, _)| {
            let path = path.strip_prefix("xtask/fuzz").ok()?;
            (path != Path::new("Cargo.toml")).then(|| Path::new("state/fuzz").join(path))
        }))
        .chain(std::iter::once(PathBuf::from("state/fuzz/Cargo.toml")))
        .chain(report_files()?)
        .chain(corpus_files().map(|(path, _)| path))
        .collect())
}

fn artifacts() -> BTreeSet<PathBuf> {
    Target::ALL
        .into_iter()
        .map(|target| Path::new("state/artifacts").join(target.name()))
        .collect()
}

struct Inventory {
    remaining: BTreeSet<PathBuf>,
    empty: BTreeSet<PathBuf>,
    entries: usize,
}

impl Inventory {
    fn check(&mut self, root: &Path, relative: &Path) -> Result<(), FuzzError> {
        if relative.components().count() > 16
            || !fs::symlink_metadata(root.join(relative))?.is_dir()
        {
            return Err(FuzzError::Results);
        }
        let required_empty = self.empty.remove(relative);
        for entry in read_directory(root, relative)? {
            let entry = entry?;
            self.entries += 1;
            if required_empty || self.entries > 4096 {
                return Err(FuzzError::Results);
            }
            let path = relative.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if !self
                    .remaining
                    .iter()
                    .chain(self.empty.iter())
                    .any(|file| file.starts_with(&path))
                {
                    return Err(FuzzError::Results);
                }
                self.check(root, &path)?;
            } else if !kind.is_file() || !self.remaining.remove(&path) {
                return Err(FuzzError::Results);
            }
        }
        Ok(())
    }
}

fn validate(
    capture: &CapturedInputFuzz,
    root: &Path,
    architecture: LinuxTarget,
) -> Result<Vec<TargetCompletion>, FuzzError> {
    let mut inventory = Inventory {
        remaining: expected(capture)?,
        empty: artifacts(),
        entries: 0,
    };
    inventory.check(root, Path::new(""))?;
    if !inventory.remaining.is_empty() || !inventory.empty.is_empty() {
        return Err(FuzzError::Results);
    }
    capture.check_source(&root.join("source"))?;
    capture.check_harness(&root.join("state"))?;
    for (path, bytes) in corpus_files() {
        if read_under(root, &path, MAX_INPUT)? != bytes {
            return Err(FuzzError::Results);
        }
    }
    let mut runs = Vec::new();
    for phase in phases(Scope::All)? {
        let bytes = read_under(
            root,
            &Path::new("state").join(format!("{}.txt", phase.label())),
            4 * 1024 * 1024,
        )?;
        if let Some(run) = validate_phase(phase, &bytes, architecture)? {
            runs.push(run);
        }
    }
    Ok(runs)
}

fn preserve(work: &WorkingFuzz, root: &Path) -> Result<(), FuzzError> {
    fs::create_dir(root)?;
    work.capture
        .write(&root.join("source"), &root.join("state"))?;
    for path in report_files()? {
        write_new(
            &root.join(&path),
            &read_under(&work.directory, &path, 4 * 1024 * 1024)?,
        )?;
    }
    for (path, _) in corpus_files() {
        write_new(
            &root.join(&path),
            &read_under(&work.directory, &path, MAX_INPUT)?,
        )?;
    }
    for path in artifacts() {
        if read_directory(&work.directory, &path)?
            .next()
            .transpose()?
            .is_some()
        {
            return Err(FuzzError::Results);
        }
        fs::create_dir_all(root.join(path))?;
    }
    Ok(())
}

impl CapturedInputFuzz {
    pub(crate) fn verify_retained(
        self,
        image: &str,
        architecture: LinuxTarget,
        root: &Path,
    ) -> Result<(InputFuzzOutcome, FuzzExecution), FuzzError> {
        let mut supervisor = CommandSupervisor::new().map_err(FuzzError::Process)?;
        let work = match WorkingFuzz::new(self, Scope::All, image, architecture) {
            Ok(work) => work,
            Err(error) => {
                return super::follow(
                    Err(error),
                    supervisor.finish().map_err(FuzzError::Process),
                    "input fuzz supervisor cleanup failed",
                )
            }
        };
        let operation = work.execute(
            &mut |command, budget, mode| match mode {
                CommandMode::Work => supervisor.run(command, budget),
                CommandMode::Cleanup => supervisor.run_cleanup(command, budget),
            },
            &mut io::stderr(),
        );
        let operation = super::follow(
            operation,
            supervisor.finish().map_err(FuzzError::Process),
            "input fuzz supervisor cleanup failed",
        );
        let operation = super::follow(
            operation,
            work.check(),
            "retained input fuzz final input check failed",
        );
        let operation = operation.and_then(|runs| {
            preserve(&work, root)?;
            validate(&work.capture, root, architecture)?;
            Ok(runs)
        });
        let execution = FuzzExecution {
            identity: work.identity.clone(),
            directory: work.directory.clone(),
        };
        let checked = work.close(operation)?;
        let outcome =
            checked
                .identity
                .capture
                .read_retained(image, architecture, root, &execution)?;
        Ok((outcome, execution))
    }

    pub(crate) fn read_retained(
        self,
        image: &str,
        architecture: LinuxTarget,
        root: &Path,
        execution: &FuzzExecution,
    ) -> Result<InputFuzzOutcome, FuzzError> {
        execution.check(image, architecture)?;
        let image = ImageIdentity::parse(image).ok_or(FuzzError::Plan)?;
        let runs = validate(&self, root, architecture)?;
        complete_all(CheckedFuzz {
            identity: FuzzIdentity {
                capture: self,
                image,
                architecture,
            },
            scope: Scope::All,
            runs,
        })
    }
}
