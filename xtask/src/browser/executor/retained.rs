use super::{
    copy_tree, finish, read as native_bytes, run_prepared_with, version, BrowserCommand,
    BrowserOutcome, PreparedBrowserRun, RunError,
};
use crate::{
    browser::{
        runtime_inputs::RuntimeInputs, validate_execution, validate_inventory, BrowserInventory,
        Phase, RunPlan, SourceSnapshot,
    },
    process::{CommandSupervisor, ProcessError},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const SCREENSHOTS: [&str; 7] = [
    "browser-320.png",
    "browser-768.png",
    "browser-1280.png",
    "browser-landscape.png",
    "browser-enlarged.png",
    "browser-enlarged-spacing.png",
    "browser-forced-colors.png",
];

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrowserExecution {
    identity: String,
    source: PathBuf,
    state: PathBuf,
}

fn invalid(reason: &'static str) -> RunError {
    io::Error::new(io::ErrorKind::InvalidData, reason).into()
}

pub(super) fn comparison_profile(
    bytes: &[u8],
    source: &str,
    image: &str,
    reference: &str,
) -> Result<(), RunError> {
    let selected =
        crate::reference::ReferenceImage::parse_inspection(reference).map_err(io::Error::other)?;
    if selected.as_str() != reference
        || bytes != format!("source={source}\nbrowser={image}\nasterisk={reference}\n").as_bytes()
    {
        return Err(invalid("paired browser source or immutable images differ"));
    }
    Ok(())
}

pub(super) fn comparison_results(
    directory: &Path,
    inventory: &BrowserInventory,
) -> Result<(), RunError> {
    native_bytes(&directory.join("reference-run.txt"))?;
    validate_execution(
        &native_bytes(&directory.join("reference-browser-results.json"))?,
        inventory,
    )?;
    artifacts(&directory.join("reference-screenshots"))
}

impl BrowserExecution {
    pub(crate) fn read_selected(
        &self,
        snapshot: SourceSnapshot,
        image: &str,
        reference: Option<&str>,
        directory: &Path,
    ) -> Result<BrowserOutcome, RunError> {
        let plan = self.plan(image)?;
        let inventory = results(directory)?;
        if let Some(reference) = reference {
            comparison_profile(
                &native_bytes(&directory.join("reference-images.txt"))?,
                &snapshot.digest(),
                image,
                reference,
            )?;
            comparison_results(directory, &inventory)?;
        } else {
            for name in [
                "reference-images.txt",
                "reference-browser-results.json",
                "reference-run.txt",
                "reference-screenshots",
            ] {
                match fs::symlink_metadata(directory.join(name)) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                    Ok(_) => {
                        return Err(invalid(
                            "paired browser results require a reference selection",
                        ))
                    }
                }
            }
        }
        Ok(BrowserOutcome {
            snapshot,
            plan,
            inventory,
            artifacts: directory.to_owned(),
            reference: reference.map(str::to_owned),
        })
    }

    fn capture(plan: &RunPlan) -> Self {
        Self {
            identity: plan.identity().to_owned(),
            source: PathBuf::from(plan.source()),
            state: PathBuf::from(plan.state()),
        }
    }

    fn plan(&self, image: &str) -> Result<RunPlan, RunError> {
        let directory = self
            .source
            .parent()
            .ok_or_else(|| invalid("browser original source has no owned directory"))?;
        if directory.file_name().and_then(|value| value.to_str())
            != Some(format!("rusterisk-browser-verify-{}", self.identity).as_str())
            || self.source != directory.join("source")
            || self.state != directory.join("state")
        {
            return Err(invalid(
                "browser original execution paths differ from their owned profile",
            ));
        }
        RunPlan::new(
            &self.identity,
            self.source
                .to_str()
                .ok_or_else(|| invalid("browser original source is not UTF-8"))?,
            self.state
                .to_str()
                .ok_or_else(|| invalid("browser original state is not UTF-8"))?,
            image,
        )
        .map_err(RunError::Plan)
    }
}

pub(crate) struct RetainedBrowserRun {
    execution: BrowserExecution,
}

impl RetainedBrowserRun {
    #[cfg(test)]
    pub(super) fn execution(&self) -> &BrowserExecution {
        &self.execution
    }

    pub(crate) fn into_execution(self) -> BrowserExecution {
        self.execution
    }
}

pub(super) trait BrowserPort {
    fn run(
        &mut self,
        command: &mut Command,
        budget: Duration,
        mode: BrowserCommand,
    ) -> Result<(), ProcessError>;
    fn finish(self) -> Result<(), ProcessError>;
}

impl BrowserPort for CommandSupervisor {
    fn run(
        &mut self,
        command: &mut Command,
        budget: Duration,
        mode: BrowserCommand,
    ) -> Result<(), ProcessError> {
        match mode {
            BrowserCommand::Run => CommandSupervisor::run(self, command, budget),
            BrowserCommand::Cleanup => CommandSupervisor::run_cleanup(self, command, budget),
        }
    }

    fn finish(self) -> Result<(), ProcessError> {
        CommandSupervisor::finish(self)
    }
}

fn png(path: &Path) -> Result<(), RunError> {
    let bytes = native_bytes(path)?;
    if bytes.len() < 33
        || bytes[..8] != [137, 80, 78, 71, 13, 10, 26, 10]
        || bytes[8..12] != 13u32.to_be_bytes()
        || &bytes[12..16] != b"IHDR"
        || bytes[16..20] == [0; 4]
        || bytes[20..24] == [0; 4]
    {
        return Err(invalid(
            "browser screenshot differs from its native PNG format",
        ));
    }
    Ok(())
}

fn screenshot_tree(
    root: &Path,
    relative: &Path,
    remaining: &mut BTreeSet<String>,
    entries: &mut usize,
    depth: usize,
) -> Result<(), RunError> {
    if depth > 8 || !fs::symlink_metadata(root.join(relative))?.is_dir() {
        return Err(invalid(
            "browser screenshot directory differs from its bounded profile",
        ));
    }
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        *entries += 1;
        if *entries > 128 {
            return Err(invalid("browser screenshot inventory exceeds its limit"));
        }
        let path = relative.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            screenshot_tree(root, &path, remaining, entries, depth + 1)?;
        } else if !kind.is_file() {
            return Err(invalid(
                "browser screenshot inventory contains a link or special file",
            ));
        } else if path.extension().is_some_and(|extension| extension == "png") {
            let name = entry.file_name();
            if !name.to_str().is_some_and(|name| remaining.remove(name)) {
                return Err(invalid(
                    "browser screenshots are duplicate or outside the native inventory",
                ));
            }
            png(&root.join(path))?;
        } else if !(path == Path::new(".last-run.json") || path.starts_with("runtime")) {
            return Err(invalid("browser screenshot inventory has additional files"));
        }
    }
    Ok(())
}

fn artifacts(root: &Path) -> Result<(), RunError> {
    let mut remaining = SCREENSHOTS.into_iter().map(str::to_owned).collect();
    screenshot_tree(root, Path::new(""), &mut remaining, &mut 0, 0)?;
    if !remaining.is_empty() {
        return Err(invalid("browser screenshot inventory is incomplete"));
    }
    RuntimeInputs::read(&root.join("runtime"))?;
    Ok(())
}

fn results(directory: &Path) -> Result<BrowserInventory, RunError> {
    for phase in Phase::ALL {
        let bytes = native_bytes(&directory.join(format!("{}.txt", phase.label())))?;
        match phase {
            Phase::RustVersion
            | Phase::CargoVersion
            | Phase::NodeVersion
            | Phase::NpmVersion
            | Phase::BrowserVersion => version(phase, &bytes)?,
            Phase::Fetch
            | Phase::Install
            | Phase::Types
            | Phase::Check
            | Phase::Unit
            | Phase::Bundle
            | Phase::Compile
            | Phase::Inventory
            | Phase::Run => {}
        }
    }
    let inventory = validate_inventory(&native_bytes(&directory.join("browser-inventory.json"))?)?;
    validate_execution(
        &native_bytes(&directory.join("browser-results.json"))?,
        &inventory,
    )?;
    artifacts(&directory.join("screenshots"))?;
    Ok(inventory)
}

pub(crate) fn read(
    snapshot: SourceSnapshot,
    image: &str,
    directory: &Path,
    execution: &BrowserExecution,
) -> Result<BrowserOutcome, RunError> {
    execution.read_selected(snapshot, image, None, directory)
}

pub(super) fn execute_with(
    prepared: PreparedBrowserRun,
    destination: &Path,
    mut port: impl BrowserPort,
) -> Result<RetainedBrowserRun, RunError> {
    let operation = run_prepared_with(prepared, |command, budget, mode| {
        port.run(command, budget, mode)
    });
    let completed = finish(operation, port.finish())?;
    let execution = BrowserExecution::capture(&completed.plan);
    let image = completed.plan.image().to_owned();
    let reference = completed.reference;
    copy_tree(&completed.artifacts, destination, 0, &mut 0, &mut 0)?;
    execution.read_selected(
        completed.snapshot,
        &image,
        reference.as_deref(),
        destination,
    )?;
    Ok(RetainedBrowserRun { execution })
}

pub(crate) fn run(
    prepared: PreparedBrowserRun,
    destination: &Path,
) -> Result<RetainedBrowserRun, RunError> {
    execute_with(
        prepared,
        destination,
        CommandSupervisor::new().map_err(RunError::Process)?,
    )
}
