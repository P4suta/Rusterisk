use super::{validate_results, CapturedCoreProofs, CoreProofPlan, ProofError, HARNESSES};
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
enum CommandMode {
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
    println!("Kani 0.68.0: {} harnesses; snapshot the production core; temporary source <= 8 MiB; native export <= 128 MiB; accepted report <= 4 MiB",HARNESSES.len());
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

fn native(path: &Path) -> Result<(), ProofError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
        return Err(ProofError::Results);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    validate_results(&bytes)
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
        let operation = combine(result.map_err(ProofError::Process), cleanup);
        match (operation, captured.check(&source)) {
            (Ok(()), Ok(())) => {}
            (Err(error), Ok(())) | (Ok(()), Err(error)) => return Err(error),
            (Err(operation), Err(error)) => {
                return Err(ProofError::FollowUp {
                    operation: operation.to_string(),
                    context: "captured proof source check failed",
                    follow_up: error.to_string(),
                })
            }
        }
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
