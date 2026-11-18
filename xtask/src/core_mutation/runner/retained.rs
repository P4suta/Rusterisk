use super::{
    follow, phase_plan, read, tools, CapturedCoreMutation, CommandMode, CommandSupervisor,
    CoreMutationError, CoreMutationOutcome, FieldMutation, ImageIdentity, MutationInventory,
    NativeFiles, Phase, WorkingMutation, PHASES,
};
use crate::process::ProcessError;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    time::Duration,
};

#[cfg(test)]
mod tests;

trait MutationPort {
    fn run(
        &mut self,
        command: &mut Command,
        budget: Duration,
        state: &Path,
    ) -> Result<(), ProcessError>;
    fn run_cleanup(
        &mut self,
        command: &mut Command,
        budget: Duration,
        state: &Path,
    ) -> Result<(), ProcessError>;
    fn finish(self) -> Result<(), ProcessError>;
}

impl MutationPort for CommandSupervisor {
    fn run(
        &mut self,
        command: &mut Command,
        budget: Duration,
        _state: &Path,
    ) -> Result<(), ProcessError> {
        CommandSupervisor::run(self, command, budget)
    }

    fn run_cleanup(
        &mut self,
        command: &mut Command,
        budget: Duration,
        _state: &Path,
    ) -> Result<(), ProcessError> {
        CommandSupervisor::run_cleanup(self, command, budget)
    }

    fn finish(self) -> Result<(), ProcessError> {
        CommandSupervisor::finish(self)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MutationExecution {
    identity: String,
    directory: PathBuf,
    exits: [i32; 6],
}

impl MutationExecution {
    pub(crate) fn check(&self, image: &str) -> Result<(), CoreMutationError> {
        let text = self.directory.to_str().ok_or(CoreMutationError::Plan)?;
        if !self.directory.is_absolute()
            || text.len() > 4096
            || text
                .chars()
                .any(|value| value.is_control() || matches!(value, '\\' | ':' | ','))
            || text
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
                != Some(format!("rusterisk-core-mutation-{}", self.identity).as_str())
            || self.exits != [0, 0, 0, 101, 0, 101]
        {
            return Err(CoreMutationError::Plan);
        }
        for phase in PHASES {
            phase_plan(&self.identity, &self.directory, image, phase)?;
        }
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
        .chain(std::iter::once(
            "results/mutants.out/outcomes.json".to_owned(),
        ))
}

fn reports(work: &Path, root: &Path) -> Result<(), CoreMutationError> {
    fs::create_dir(root)?;
    fs::create_dir(root.join("state"))?;
    fs::create_dir(root.join("state/tools"))?;
    fs::create_dir_all(root.join("state/results/mutants.out"))?;
    for name in report_files() {
        let bytes = match read(&work.join("state"), Path::new(&name)) {
            Ok(bytes) => bytes,
            Err(CoreMutationError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                continue
            }
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

fn working_paths<'a>(
    capture: &'a CapturedCoreMutation,
    root: &'a Path,
) -> impl Iterator<Item = PathBuf> + 'a {
    capture
        .inputs()
        .filter_map(move |(path, _)| {
            let path = path.strip_prefix("crates/core").ok()?;
            (path.starts_with("src") || path.starts_with("tests")).then(|| root.join(path))
        })
        .chain([root.join("Cargo.toml"), root.join("Cargo.lock")])
}

fn expected(capture: &CapturedCoreMutation) -> BTreeSet<PathBuf> {
    capture
        .inputs()
        .map(|(path, _)| Path::new("original").join(path))
        .chain(working_paths(capture, Path::new("source")))
        .chain(
            FieldMutation::ALL
                .into_iter()
                .flat_map(|mutation| working_paths(capture, Path::new(mutation.label()))),
        )
        .chain(report_files().map(|name| Path::new("state").join(name)))
        .collect()
}

fn check_files(
    root: &Path,
    relative: &Path,
    remaining: &mut BTreeSet<PathBuf>,
) -> Result<(), CoreMutationError> {
    if relative.components().count() > 12 || !fs::symlink_metadata(root.join(relative))?.is_dir() {
        return Err(CoreMutationError::Plan);
    }
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let path = relative.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if !remaining.iter().any(|file| file.starts_with(&path)) {
                return Err(CoreMutationError::Plan);
            }
            check_files(root, &path, remaining)?;
        } else if !kind.is_file() || !remaining.remove(&path) {
            return Err(CoreMutationError::Plan);
        }
    }
    Ok(())
}

fn validate(
    capture: &CapturedCoreMutation,
    root: &Path,
) -> Result<MutationInventory, CoreMutationError> {
    let mut remaining = expected(capture);
    check_files(root, Path::new(""), &mut remaining)?;
    if !remaining.is_empty() {
        return Err(CoreMutationError::Plan);
    }
    capture.check(&root.join("original"), &root.join("source"))?;
    for mutation in FieldMutation::ALL {
        capture.check_field(
            &root.join("original"),
            &root.join(mutation.label()),
            mutation,
        )?;
    }
    let state = root.join("state");
    tools::check(&state)?;
    for phase in PHASES {
        read(&state, Path::new(&format!("{}.stderr.txt", phase.label())))?;
        let bytes = read(&state, Path::new(&format!("{}.txt", phase.label())))?;
        if matches!(phase, Phase::FieldTests(_)) {
            crate::core_assurance::validate_field_execution(
                &bytes,
                capture.tests(),
                capture.targets().len(),
            )?;
        }
    }
    let inventory = crate::core_assurance::validate_mutation_inventory(&read(
        &state,
        Path::new("mutation-inventory.txt"),
    )?)?;
    crate::core_assurance::validate_mutation_campaign(
        &read(&state, Path::new("results/mutants.out/outcomes.json"))?,
        &inventory,
        &FieldMutation::ALL,
    )?;
    Ok(inventory)
}

fn verify_retained_with(
    capture: CapturedCoreMutation,
    image: &str,
    root: &Path,
    mut port: impl MutationPort,
) -> Result<(CoreMutationOutcome, MutationExecution), CoreMutationError> {
    let work = match WorkingMutation::new(capture, image) {
        Ok(work) => work,
        Err(error) => {
            return follow(
                Err(error),
                port.finish().map_err(CoreMutationError::Process),
                "mutation supervisor cleanup failed",
            )
        }
    };
    let state = work.directory.join("state");
    let operation = work.execute(
        NativeFiles::Retained,
        &mut |command, budget, mode| match mode {
            CommandMode::Work => port.run(command, budget, &state),
            CommandMode::Cleanup => port.run_cleanup(command, budget, &state),
        },
    );
    let operation = follow(
        operation,
        port.finish().map_err(CoreMutationError::Process),
        "mutation supervisor cleanup failed",
    );
    let operation = follow(operation, work.check(), "mutation final input check failed");
    let operation = follow(
        operation,
        reports(&work.directory, root),
        "mutation native report preservation failed",
    );
    let operation = operation.and_then(|native| {
        work.capture
            .write(&root.join("original"), &root.join("source"))?;
        for mutation in FieldMutation::ALL {
            work.capture
                .write_field(&root.join(mutation.label()), mutation)?;
        }
        validate(&work.capture, root)?;
        Ok(native)
    });
    let execution = operation.as_ref().ok().map(|native| MutationExecution {
        identity: work.identity.clone(),
        directory: work.directory.clone(),
        exits: native.exits,
    });
    let checked = work.close(operation)?;
    let execution = execution.ok_or(CoreMutationError::Plan)?;
    let outcome = checked
        .capture
        .read_retained(checked.image.as_str(), root, &execution)?;
    Ok((outcome, execution))
}

impl CapturedCoreMutation {
    pub(crate) fn verify_retained(
        self,
        image: &str,
        root: &Path,
    ) -> Result<(CoreMutationOutcome, MutationExecution), CoreMutationError> {
        let supervisor = CommandSupervisor::new().map_err(CoreMutationError::Process)?;
        verify_retained_with(self, image, root, supervisor)
    }

    pub(crate) fn read_retained(
        self,
        image: &str,
        root: &Path,
        execution: &MutationExecution,
    ) -> Result<CoreMutationOutcome, CoreMutationError> {
        execution.check(image)?;
        let image = ImageIdentity::parse(image).ok_or(CoreMutationError::Plan)?;
        let inventory = validate(&self, root)?;
        Ok(CoreMutationOutcome {
            capture: self,
            inventory,
            image,
        })
    }
}
