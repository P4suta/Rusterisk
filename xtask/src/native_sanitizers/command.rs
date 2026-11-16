use super::runner::CheckedNative;
use super::{
    CapturedNative, CommandMode, Defect, NativeError, NativeInventory, NativeLibrary,
    NativeSanitizerPlan, Phase, Scope,
};
use crate::process::{CommandSupervisor, ProcessError};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub enum Execution {
    DryRun,
    Run,
}

/// Synthetic command ports don't produce completed native outcomes.
///
/// ```compile_fail
/// use std::path::Path;
/// use xtask::native_sanitizers::{CapturedNative, NativeError, NativeOutcome};
/// fn synthetic(capture: &CapturedNative, destination: &Path, image: &str) -> Result<NativeOutcome, NativeError> {
///     capture.run_with(destination, image, |_, _, _| Ok(()))
/// }
/// ```
pub struct NativeOutcome {
    capture: CapturedNative,
    checked: CheckedNative,
    artifacts: PathBuf,
}

impl NativeOutcome {
    pub fn inputs(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.capture.inputs()
    }
    pub fn image(&self) -> &str {
        self.checked.plan.image()
    }
    pub fn inventory(&self, scope: Scope) -> &NativeInventory {
        match scope {
            Scope::Media => &self.checked.inventories[0],
            Scope::Storage => &self.checked.inventories[1],
            Scope::Tls => &self.checked.inventories[2],
        }
    }
    pub const fn test_count(&self) -> usize {
        self.checked.total
    }
    pub fn artifacts(&self) -> &Path {
        &self.artifacts
    }
}

impl CapturedNative {
    pub fn verify(self, destination: &Path, image: &str) -> Result<NativeOutcome, NativeError> {
        let plan = plan(image)?;
        let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
        let operation = self.run_checked_with(
            destination,
            plan.image(),
            |command, budget, mode| match mode {
                CommandMode::Work => supervisor.run(command, budget),
                CommandMode::Cleanup => supervisor.run_cleanup(command, budget),
            },
        );
        finish(self, operation, supervisor.finish(), destination.to_owned())
    }
}

fn finish(
    capture: CapturedNative,
    operation: Result<CheckedNative, NativeError>,
    cleanup: Result<(), ProcessError>,
    artifacts: PathBuf,
) -> Result<NativeOutcome, NativeError> {
    match (operation, cleanup) {
        (Ok(checked), Ok(())) => Ok(NativeOutcome {
            capture,
            checked,
            artifacts,
        }),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(io::Error::other(error).into()),
        (Err(operation), Err(error)) => Err(NativeError::FollowUp {
            operation: operation.to_string(),
            context: "native supervisor cleanup failed",
            error: io::Error::other(error),
        }),
    }
}

pub fn plan(image: &str) -> Result<NativeSanitizerPlan, NativeError> {
    let directory = std::env::temp_dir().join("rusterisk-native-sanitizers-preview");
    NativeSanitizerPlan::new(
        "preview",
        directory
            .join("source")
            .to_str()
            .ok_or_else(|| io::Error::other("native source path encoding"))?,
        directory
            .join("state")
            .to_str()
            .ok_or_else(|| io::Error::other("native state path encoding"))?,
        image,
    )
    .map_err(NativeError::Io)
}

fn preview(capture: &CapturedNative, image: &str) -> Result<(), NativeError> {
    let plan = plan(image)?;
    println!(
        "native sanitizers: source={} scopes=media,storage,tls",
        capture.digest()
    );
    for phase in [
        Phase::ClangVersion,
        Phase::RustVersion,
        Phase::CompileProbe(Defect::Address),
        Phase::RejectProbe(Defect::Address),
        Phase::CompileProbe(Defect::Undefined),
        Phase::RejectProbe(Defect::Undefined),
        Phase::Fetch,
        Phase::Build(Scope::Media),
        Phase::Build(Scope::Storage),
        Phase::Build(Scope::Tls),
        Phase::InspectArchive(NativeLibrary::Opus),
        Phase::InspectArchive(NativeLibrary::Sqlite),
        Phase::InspectArchive(NativeLibrary::AwsLc),
        Phase::Inventory(Scope::Media),
        Phase::Run(Scope::Media),
        Phase::Inventory(Scope::Storage),
        Phase::Run(Scope::Storage),
        Phase::Inventory(Scope::Tls),
        Phase::Run(Scope::Tls),
    ] {
        let step = plan.step(phase);
        println!(
            "phase={} budget={}s docker {}",
            phase.label(),
            step.budget.as_secs(),
            step.arguments.join(" ")
        );
        let cleanup = plan.cleanup(phase);
        println!(
            "interruption cleanup: budget={}s docker {}",
            cleanup.budget.as_secs(),
            cleanup.arguments.join(" ")
        );
    }
    Ok(())
}

fn directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(io::Error::other(
            "native result parent must be a regular directory",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(path),
        Err(error) => Err(error),
    }
}

pub fn execute(root: &Path, execution: Execution, image: &str) -> Result<(), NativeError> {
    let capture = CapturedNative::read(root)?;
    if matches!(execution, Execution::DryRun) {
        return preview(&capture, image);
    }
    let mut destination = std::path::absolute(root)?;
    for component in [".cache", "native-sanitizers"] {
        destination.push(component);
        directory(&destination)?;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    destination.push(format!("{}-{stamp}", std::process::id()));
    println!(
        "native sanitizers: source={} results={}",
        capture.digest(),
        destination.display()
    );
    io::stdout().flush()?;
    let outcome = capture.verify(&destination, image)?;
    println!(
        "native sanitizers: {} boundary tests passed",
        outcome.test_count()
    );
    Ok(())
}
