use super::{CapturedCoreMiri, CoreMiriError, Execution};
use crate::{
    core_assurance::{
        validate_miri_discovery, validate_miri_execution, CoreAssurancePlan, MiriInventory, Phase,
    },
    process::{CommandSupervisor, ProcessError},
    reference::ImageIdentity,
};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const PHASES: [Phase; 3] = [Phase::MiriSetup, Phase::MiriInventory, Phase::MiriTests];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandMode {
    Work,
    Cleanup,
}

/// Planning doesn't produce a completed Miri outcome.
///
/// ```compile_fail
/// use xtask::core_miri::{CoreMiriOutcome, CoreMiriError, Execution, execute};
/// fn synthetic(image: &str) -> Result<CoreMiriOutcome, CoreMiriError> {
///     execute(Execution::DryRun, image)
/// }
/// ```
pub struct CoreMiriOutcome {
    capture: CapturedCoreMiri,
    inventory: MiriInventory,
    image: ImageIdentity,
}
impl CoreMiriOutcome {
    pub fn inputs(&self) -> impl ExactSizeIterator<Item = (&Path, &[u8])> {
        self.capture.inputs()
    }
    pub fn image(&self) -> &str {
        self.image.as_str()
    }
    pub fn inventory(&self) -> &MiriInventory {
        &self.inventory
    }
    pub fn test_count(&self) -> usize {
        self.capture.tests().len()
    }
}
pub fn plan(image: &str) -> Result<CoreAssurancePlan, CoreMiriError> {
    let root = std::env::temp_dir().join("rusterisk-core-miri-preview");
    Ok(CoreAssurancePlan::new(
        "preview",
        root.join("source").to_str().ok_or(CoreMiriError::Plan)?,
        root.join("state").to_str().ok_or(CoreMiriError::Plan)?,
        image,
    )?)
}
impl CapturedCoreMiri {
    pub fn verify(self, image: &str) -> Result<CoreMiriOutcome, CoreMiriError> {
        let mut supervisor = CommandSupervisor::new().map_err(CoreMiriError::Process)?;
        let operation = verify_with(self, image, |command, budget, mode| match mode {
            CommandMode::Work => supervisor.run(command, budget),
            CommandMode::Cleanup => supervisor.run_cleanup(command, budget),
        });
        finish(operation, supervisor.finish())
    }
}
struct CheckedMiri {
    capture: CapturedCoreMiri,
    inventory: MiriInventory,
    image: ImageIdentity,
}
fn follow<T>(
    operation: Result<T, CoreMiriError>,
    cleanup: Result<(), CoreMiriError>,
    context: &'static str,
) -> Result<T, CoreMiriError> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(follow_up)) => Err(CoreMiriError::FollowUp {
            operation: operation.to_string(),
            context,
            follow_up: follow_up.to_string(),
        }),
    }
}
fn finish(
    operation: Result<CheckedMiri, CoreMiriError>,
    cleanup: Result<(), ProcessError>,
) -> Result<CoreMiriOutcome, CoreMiriError> {
    follow(
        operation,
        cleanup.map_err(CoreMiriError::Process),
        "Miri supervisor cleanup failed",
    )
    .map(|checked| CoreMiriOutcome {
        capture: checked.capture,
        inventory: checked.inventory,
        image: checked.image,
    })
}
fn reserve(parent: &Path, identity: &str) -> Result<(String, PathBuf), CoreMiriError> {
    for attempt in 0..32 {
        let identity = if attempt == 0 {
            identity.to_owned()
        } else {
            format!("{identity}-{attempt}")
        };
        let directory = parent.join(format!("rusterisk-core-miri-{identity}"));
        match std::fs::create_dir(&directory) {
            Ok(()) => return Ok((identity, directory)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Miri directory reservation limit exceeded",
    )
    .into())
}
fn read(path: &Path) -> Result<Vec<u8>, CoreMiriError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
        return Err(CoreMiriError::Plan);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(CoreMiriError::Plan);
    }
    Ok(bytes)
}
fn print(capture: &CapturedCoreMiri, plan: &CoreAssurancePlan) {
    println!("core Miri: {} integration targets; {} tests; source snapshot <= 8 MiB; native reports <= 4 MiB",capture.targets().len(),capture.tests().len());
    for phase in PHASES {
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
}
fn verify_with(
    capture: CapturedCoreMiri,
    image: &str,
    mut perform: impl FnMut(&mut Command, Duration, CommandMode) -> Result<(), ProcessError>,
) -> Result<CheckedMiri, CoreMiriError> {
    let image = ImageIdentity::parse(image).ok_or(CoreMiriError::Plan)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let (identity, directory) = reserve(
        &std::env::temp_dir(),
        &format!("miri-{}-{stamp}", std::process::id()),
    )?;
    let original = directory.join("original");
    let source = directory.join("source");
    let state = directory.join("state");
    let operation = (|| {
        capture.write(&original, &source)?;
        for name in ["cargo", "tmp", "results"] {
            std::fs::create_dir_all(state.join(name))?;
        }
        let plan = CoreAssurancePlan::new(
            &identity,
            source.to_str().ok_or(CoreMiriError::Plan)?,
            state.to_str().ok_or(CoreMiriError::Plan)?,
            image.as_str(),
        )?;
        print(&capture, &plan);
        let mut inventory = None;
        for phase in PHASES {
            let path = state.join(format!("{}.txt", phase.label()));
            let output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            let step = plan.step(phase);
            let result = perform(
                Command::new("docker").args(step.arguments).stdout(output),
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
                    let step = plan.cleanup(phase);
                    perform(
                        Command::new("docker").args(step.arguments),
                        step.budget,
                        CommandMode::Cleanup,
                    )
                }
            };
            let result = follow(
                result.map_err(|source| CoreMiriError::Phase { phase, source }),
                cleanup.map_err(CoreMiriError::Process),
                "Miri container cleanup failed",
            );
            follow(
                result,
                capture.check(&original, &source),
                "Miri input check failed",
            )?;
            let bytes = read(&path)?;
            match phase {
                Phase::MiriSetup => {}
                Phase::MiriInventory => {
                    inventory = Some(validate_miri_discovery(
                        &bytes,
                        capture.tests(),
                        capture.targets().len(),
                    )?)
                }
                Phase::MiriTests => {
                    validate_miri_execution(&bytes, inventory.as_ref().ok_or(CoreMiriError::Plan)?)?
                }
                Phase::MutationInventory
                | Phase::Mutations
                | Phase::FieldBuild(_)
                | Phase::FieldTests(_) => return Err(CoreMiriError::Plan),
            }
        }
        inventory.ok_or(CoreMiriError::Plan)
    })();
    follow(
        operation,
        std::fs::remove_dir_all(directory).map_err(CoreMiriError::Io),
        "Miri temporary cleanup failed",
    )
    .map(|inventory| CheckedMiri {
        capture,
        inventory,
        image,
    })
}
pub fn execute(execution: Execution, image: &str) -> Result<(), CoreMiriError> {
    let capture = CapturedCoreMiri::read(&std::env::current_dir()?)?;
    match execution {
        Execution::DryRun => {
            print(&capture, &plan(image)?);
            Ok(())
        }
        Execution::Run => {
            let result = capture.verify(image)?;
            println!(
                "core Miri: {} tests passed without skipped tests",
                result.test_count()
            );
            Ok(())
        }
    }
}
#[cfg(test)]
mod tests;
