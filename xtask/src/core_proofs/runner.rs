use super::{
    validate_results, CapturedCoreProofs, CoreProofPlan, ProofError, HARNESSES, RESULTS_BYTE_LIMIT,
};
use crate::process::{CommandSupervisor, ProcessError};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy)]
pub enum Execution {
    DryRun,
    Run,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandMode {
    Work,
    Cleanup,
}

/// Completed proof outcomes require the actual supervised execution path.
///
/// ```compile_fail
/// use xtask::core_proofs::{CoreProofOutcome, ProofError, Execution, execute};
/// fn synthetic(image: &str) -> Result<CoreProofOutcome, ProofError> {
///     execute(Execution::DryRun, image)
/// }
/// ```
pub struct CoreProofOutcome {
    captured: CapturedCoreProofs,
    plan: CoreProofPlan,
}

impl CoreProofOutcome {
    pub fn inputs(&self) -> impl ExactSizeIterator<Item = (&Path, &[u8])> {
        self.captured.inputs()
    }
    pub fn image(&self) -> &str {
        self.plan.image()
    }
    pub const fn harnesses(&self) -> &[&'static str] {
        &HARNESSES
    }
}

pub fn plan(image: &str) -> Result<CoreProofPlan, ProofError> {
    let directory = std::env::temp_dir().join("rusterisk-core-proofs-preview");
    CoreProofPlan::new(
        "preview",
        directory
            .join("source/crates/core")
            .to_str()
            .ok_or(ProofError::Plan)?,
        directory.join("results").to_str().ok_or(ProofError::Plan)?,
        image,
    )
}

impl CapturedCoreProofs {
    pub fn verify(self, image: &str) -> Result<CoreProofOutcome, ProofError> {
        let preview = plan(image)?;
        let mut supervisor = CommandSupervisor::new().map_err(ProofError::Process)?;
        let operation = verify_with(self, preview.image(), |command, budget, mode| match mode {
            CommandMode::Work => supervisor.run(command, budget),
            CommandMode::Cleanup => supervisor.run_cleanup(command, budget),
        });
        finish(operation, supervisor.finish())
    }
}

struct CheckedProof {
    captured: CapturedCoreProofs,
    plan: CoreProofPlan,
}

fn combine<T>(
    operation: Result<T, ProofError>,
    cleanup: Result<(), ProcessError>,
) -> Result<T, ProofError> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(ProofError::Process(error)),
        (Err(operation), Err(cleanup)) => Err(ProofError::Cleanup {
            operation: operation.to_string(),
            cleanup,
        }),
    }
}

fn finish(
    operation: Result<CheckedProof, ProofError>,
    cleanup: Result<(), ProcessError>,
) -> Result<CoreProofOutcome, ProofError> {
    combine(operation, cleanup).map(|checked| CoreProofOutcome {
        captured: checked.captured,
        plan: checked.plan,
    })
}

fn reserve(parent: &Path, identity: &str) -> Result<(String, PathBuf), ProofError> {
    for attempt in 0..32 {
        let identity = if attempt == 0 {
            identity.to_owned()
        } else {
            format!("{identity}-{attempt}")
        };
        let directory = parent.join(format!("rusterisk-core-proofs-{identity}"));
        match std::fs::create_dir(&directory) {
            Ok(()) => return Ok((identity, directory)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(ProofError::Io(error)),
        }
    }
    Err(ProofError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "proof directory reservation limit exceeded",
    )))
}

fn print(plan: &CoreProofPlan) {
    let step = plan.run();
    println!(
        "Kani 0.68.0: {} harnesses; snapshot the production core; temporary source <= 8 MiB; native export <= 128 MiB; accepted report <= {} MiB",
        HARNESSES.len(),
        RESULTS_BYTE_LIMIT / (1024 * 1024)
    );
    println!(
        "budget={}s docker {}",
        step.budget.as_secs(),
        step.arguments.join(" ")
    );
    let cleanup = plan.cleanup();
    println!(
        "interruption cleanup: budget={}s docker {}",
        cleanup.budget.as_secs(),
        cleanup.arguments.join(" ")
    );
}

fn native_bytes(path: &Path) -> Result<Vec<u8>, ProofError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > RESULTS_BYTE_LIMIT as u64 {
        return Err(ProofError::Results);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(RESULTS_BYTE_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    validate_results(&bytes)?;
    Ok(bytes)
}

fn native(path: &Path) -> Result<(), ProofError> {
    native_bytes(path).map(|_| ())
}

fn perform_plan(
    plan: &CoreProofPlan,
    perform: &mut impl FnMut(&mut Command, Duration, CommandMode) -> Result<(), ProcessError>,
) -> Result<(), ProofError> {
    let step = plan.run();
    let result = perform(
        Command::new("docker").args(&step.arguments),
        step.budget,
        CommandMode::Work,
    );
    let cleanup = match &result {
        Ok(()) | Err(ProcessError::Status(_)) => Ok(()),
        Err(
            ProcessError::Io(_)
            | ProcessError::Timeout
            | ProcessError::InvalidBudget
            | ProcessError::InvalidOutput
            | ProcessError::Interrupted
            | ProcessError::UnsupportedSupervision
            | ProcessError::Cleanup { .. },
        ) => {
            let step = plan.cleanup();
            perform(
                Command::new("docker").args(&step.arguments),
                step.budget,
                CommandMode::Cleanup,
            )
        }
    };
    combine(result.map_err(ProofError::Process), cleanup)
}

fn follow<T>(
    operation: Result<T, ProofError>,
    follow_up: Result<(), ProofError>,
    context: &'static str,
) -> Result<T, ProofError> {
    match (operation, follow_up) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(error)) => Err(ProofError::FollowUp {
            operation: operation.to_string(),
            context,
            follow_up: error.to_string(),
        }),
    }
}

#[cfg(unix)]
pub(crate) struct RetainedProof {
    checked: CheckedProof,
    source: PathBuf,
    results: PathBuf,
}

#[cfg(unix)]
impl RetainedProof {
    pub(crate) fn source_root(&self) -> &Path {
        Path::new(&self.checked.plan.source)
    }

    pub(crate) fn results_root(&self) -> &Path {
        &self.results
    }

    pub(crate) fn check(&self) -> Result<(), ProofError> {
        follow(
            self.checked.captured.check(&self.source),
            native(&self.results.join("kani.json")),
            "retained proof output check failed",
        )
    }

    pub(crate) fn preserve(&self, directory: &Path) -> Result<(), ProofError> {
        use std::io::Write;
        self.checked
            .captured
            .write(&directory.join("core-source"))?;
        let results = directory.join("results");
        std::fs::create_dir(&results)?;
        let bytes = native_bytes(&self.results.join("kani.json"))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(results.join("kani.json"))?;
        file.write_all(&bytes)?;
        file.flush()?;
        Ok(())
    }

    pub(crate) fn into_outcome(self) -> CoreProofOutcome {
        CoreProofOutcome {
            captured: self.checked.captured,
            plan: self.checked.plan,
        }
    }

    pub(crate) fn check_preserved(&self, directory: &Path) -> Result<(), ProofError> {
        follow(
            self.checked.captured.check(&directory.join("core-source")),
            native(&directory.join("results/kani.json")),
            "preserved proof output check failed",
        )
    }
}

#[cfg(unix)]
pub(crate) fn verify_retained(
    captured: CapturedCoreProofs,
    image: &str,
    identity: &str,
    directory: &Path,
    mut perform: impl FnMut(&mut Command, Duration, CommandMode) -> Result<(), ProcessError>,
) -> Result<RetainedProof, ProofError> {
    let source = directory.join("source");
    let results = directory.join("results");
    let plan = CoreProofPlan::new(
        identity,
        source
            .join("crates/core")
            .to_str()
            .ok_or(ProofError::Plan)?,
        results.to_str().ok_or(ProofError::Plan)?,
        image,
    )?;
    captured.write(&source)?;
    std::fs::create_dir(&results)?;
    print(&plan);
    follow(
        perform_plan(&plan, &mut perform),
        captured.check(&source),
        "captured proof source check failed",
    )?;
    native(&results.join("kani.json"))?;
    Ok(RetainedProof {
        checked: CheckedProof { captured, plan },
        source,
        results,
    })
}

#[cfg(unix)]
pub(crate) fn read_retained(
    captured: CapturedCoreProofs,
    source: &Path,
    results: &Path,
    plan: CoreProofPlan,
) -> Result<CoreProofOutcome, ProofError> {
    follow(
        captured.check(source),
        native(&results.join("kani.json")),
        "retained proof output check failed",
    )?;
    Ok(CoreProofOutcome { captured, plan })
}

fn verify_with(
    captured: CapturedCoreProofs,
    image: &str,
    mut perform: impl FnMut(&mut Command, Duration, CommandMode) -> Result<(), ProcessError>,
) -> Result<CheckedProof, ProofError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ProofError::Plan)?
        .as_nanos();
    let (identity, directory) = reserve(
        &std::env::temp_dir(),
        &format!("{}-{stamp}", std::process::id()),
    )?;
    let source = directory.join("source");
    let results = directory.join("results");
    let operation = (|| {
        captured.write(&source)?;
        std::fs::create_dir(&results)?;
        let plan = CoreProofPlan::new(
            &identity,
            source
                .join("crates/core")
                .to_str()
                .ok_or(ProofError::Plan)?,
            results.to_str().ok_or(ProofError::Plan)?,
            image,
        )?;
        print(&plan);
        follow(
            perform_plan(&plan, &mut perform),
            captured.check(&source),
            "captured proof source check failed",
        )?;
        native(&results.join("kani.json"))?;
        Ok(plan)
    })();
    combine(
        operation,
        std::fs::remove_dir_all(directory).map_err(ProcessError::Io),
    )
    .map(|plan| CheckedProof { captured, plan })
}

pub fn execute(execution: Execution, image: &str) -> Result<(), ProofError> {
    let captured = CapturedCoreProofs::read(&std::env::current_dir()?)?;
    match execution {
        Execution::DryRun => {
            print(&plan(image)?);
            Ok(())
        }
        Execution::Run => {
            let outcome = captured.verify(image)?;
            println!(
                "core proofs: {} Kani harnesses passed with reachable contracts",
                outcome.harnesses().len()
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
pub(crate) use tests::fixture as artifact_fixture;
