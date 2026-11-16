use super::{CapturedCoreMutation, CoreMutationError, Execution};
use crate::{
    core_assurance::{
        validate_field_execution, validate_mutation_campaign, validate_mutation_inventory,
        AssuranceError, CoreAssurancePlan, FieldMutation, MutationInventory, Phase,
    },
    process::{CommandSupervisor, ProcessError},
    reference::ImageIdentity,
};
use std::{
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const PHASES: [Phase; 6] = [
    Phase::MutationInventory,
    Phase::Mutations,
    Phase::FieldBuild(FieldMutation::TerminalState),
    Phase::FieldTests(FieldMutation::TerminalState),
    Phase::FieldBuild(FieldMutation::ConnectionClaim),
    Phase::FieldTests(FieldMutation::ConnectionClaim),
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandMode {
    Work,
    Cleanup,
}

/// Planning doesn't produce a completed mutation outcome.
///
/// ```compile_fail
/// use xtask::core_mutation::{CoreMutationOutcome, CoreMutationError, Execution, execute};
/// fn synthetic(image: &str) -> Result<CoreMutationOutcome, CoreMutationError> {
///     execute(Execution::DryRun, image)
/// }
/// ```
pub struct CoreMutationOutcome {
    capture: CapturedCoreMutation,
    inventory: MutationInventory,
    image: ImageIdentity,
}
impl CoreMutationOutcome {
    pub fn inputs(&self) -> impl ExactSizeIterator<Item = (&Path, &[u8])> {
        self.capture.inputs()
    }
    pub fn image(&self) -> &str {
        self.image.as_str()
    }
    pub fn inventory(&self) -> &MutationInventory {
        &self.inventory
    }
    pub fn test_count(&self) -> usize {
        self.capture.tests().len()
    }
}

pub fn plan(image: &str) -> Result<CoreAssurancePlan, CoreMutationError> {
    phase_plan(
        "preview",
        &std::env::temp_dir().join("rusterisk-core-mutation-preview"),
        image,
        Phase::MutationInventory,
    )
}

impl CapturedCoreMutation {
    pub fn verify(self, image: &str) -> Result<CoreMutationOutcome, CoreMutationError> {
        let mut supervisor = CommandSupervisor::new().map_err(CoreMutationError::Process)?;
        let operation = verify_with(self, image, |command, budget, mode| match mode {
            CommandMode::Work => supervisor.run(command, budget),
            CommandMode::Cleanup => supervisor.run_cleanup(command, budget),
        });
        finish(operation, supervisor.finish())
    }
}

struct CheckedMutation {
    capture: CapturedCoreMutation,
    inventory: MutationInventory,
    image: ImageIdentity,
}

fn follow<T>(
    operation: Result<T, CoreMutationError>,
    cleanup: Result<(), CoreMutationError>,
    context: &'static str,
) -> Result<T, CoreMutationError> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(follow_up)) => Err(CoreMutationError::FollowUp {
            operation: operation.to_string(),
            context,
            follow_up: follow_up.to_string(),
        }),
    }
}

fn finish(
    operation: Result<CheckedMutation, CoreMutationError>,
    cleanup: Result<(), ProcessError>,
) -> Result<CoreMutationOutcome, CoreMutationError> {
    follow(
        operation,
        cleanup.map_err(CoreMutationError::Process),
        "mutation supervisor cleanup failed",
    )
    .map(|checked| CoreMutationOutcome {
        capture: checked.capture,
        inventory: checked.inventory,
        image: checked.image,
    })
}

fn reserve(parent: &Path, identity: &str) -> Result<(String, PathBuf), CoreMutationError> {
    for attempt in 0..32 {
        let identity = if attempt == 0 {
            identity.to_owned()
        } else {
            format!("{identity}-{attempt}")
        };
        let directory = parent.join(format!("rusterisk-core-mutation-{identity}"));
        match fs::create_dir(&directory) {
            Ok(()) => return Ok((identity, directory)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "mutation directory reservation limit exceeded",
    )
    .into())
}

fn read(state: &Path, relative: &Path) -> Result<Vec<u8>, CoreMutationError> {
    if !fs::symlink_metadata(state)?.is_dir() {
        return Err(CoreMutationError::Plan);
    }
    let mut path = state.to_owned();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(CoreMutationError::Plan);
        };
        path.push(name);
        if components.peek().is_some() && !fs::symlink_metadata(&path)?.is_dir() {
            return Err(CoreMutationError::Plan);
        }
    }
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
        return Err(CoreMutationError::Plan);
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(CoreMutationError::Plan);
    }
    Ok(bytes)
}

fn phase_plan(
    identity: &str,
    directory: &Path,
    image: &str,
    phase: Phase,
) -> Result<CoreAssurancePlan, CoreMutationError> {
    let source = match phase {
        Phase::MutationInventory | Phase::Mutations => directory.join("source"),
        Phase::FieldBuild(mutation) | Phase::FieldTests(mutation) => {
            directory.join(mutation.label())
        }
        Phase::MiriSetup | Phase::MiriInventory | Phase::MiriTests => {
            return Err(CoreMutationError::Plan);
        }
    };
    Ok(CoreAssurancePlan::new(
        identity,
        source.to_str().ok_or(CoreMutationError::Plan)?,
        directory
            .join("state")
            .to_str()
            .ok_or(CoreMutationError::Plan)?,
        image,
    )?)
}

fn print(
    capture: &CapturedCoreMutation,
    identity: &str,
    directory: &Path,
    image: &str,
) -> Result<(), CoreMutationError> {
    println!("core mutation: {} integration targets; {} tests; 2 source-bound field mutations; source snapshot <= 8 MiB; native reports <= 4 MiB", capture.targets().len(), capture.tests().len());
    for phase in PHASES {
        let plan = phase_plan(identity, directory, image, phase)?;
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

fn run_phase(
    plan: &CoreAssurancePlan,
    phase: Phase,
    state: &Path,
    perform: &mut impl FnMut(&mut Command, Duration, CommandMode) -> Result<(), ProcessError>,
) -> Result<(), CoreMutationError> {
    let output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(state.join(format!("{}.txt", phase.label())))?;
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
    let result = match result {
        Err(ProcessError::Status(status))
            if matches!(phase, Phase::FieldTests(_)) && status.code() == Some(101) =>
        {
            Ok(())
        }
        Ok(()) => {
            if matches!(phase, Phase::FieldTests(_)) {
                Err(CoreMutationError::Assurance(AssuranceError::Results))
            } else {
                Ok(())
            }
        }
        Err(source) => Err(CoreMutationError::Phase { phase, source }),
    };
    follow(
        result,
        cleanup.map_err(CoreMutationError::Process),
        "mutation container cleanup failed",
    )
}

fn verify_with(
    capture: CapturedCoreMutation,
    image: &str,
    mut perform: impl FnMut(&mut Command, Duration, CommandMode) -> Result<(), ProcessError>,
) -> Result<CheckedMutation, CoreMutationError> {
    let image = ImageIdentity::parse(image).ok_or(CoreMutationError::Plan)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let (identity, directory) = reserve(
        &std::env::temp_dir(),
        &format!("mutation-{}-{stamp}", std::process::id()),
    )?;
    let original = directory.join("original");
    let source = directory.join("source");
    let state = directory.join("state");
    let operation = (|| {
        capture.write(&original, &source)?;
        for name in ["cargo", "tmp", "results"] {
            fs::create_dir_all(state.join(name))?;
        }
        print(&capture, &identity, &directory, image.as_str())?;
        let mut inventory = None;
        let mut campaign = None;
        for phase in PHASES {
            let plan = phase_plan(&identity, &directory, image.as_str(), phase)?;
            if let Phase::FieldBuild(mutation) = phase {
                capture.write_field(&directory.join(mutation.label()), mutation)?;
            }
            let checked = follow(
                run_phase(&plan, phase, &state, &mut perform),
                capture.check(&original, &source),
                "mutation input check failed",
            );
            let checked = match phase {
                Phase::FieldBuild(mutation) | Phase::FieldTests(mutation) => follow(
                    checked,
                    capture.check_field(&original, &directory.join(mutation.label()), mutation),
                    "field mutation input check failed",
                ),
                Phase::MutationInventory | Phase::Mutations => checked,
                Phase::MiriSetup | Phase::MiriInventory | Phase::MiriTests => {
                    return Err(CoreMutationError::Plan);
                }
            };
            checked?;
            let bytes = read(&state, Path::new(&format!("{}.txt", phase.label())))?;
            match phase {
                Phase::MutationInventory => inventory = Some(validate_mutation_inventory(&bytes)?),
                Phase::Mutations => {
                    campaign = Some(read(
                        &state,
                        Path::new("results/mutants.out/outcomes.json"),
                    )?);
                }
                Phase::FieldBuild(_) => {}
                Phase::FieldTests(_) => {
                    validate_field_execution(&bytes, capture.tests(), capture.targets().len())?;
                }
                Phase::MiriSetup | Phase::MiriInventory | Phase::MiriTests => {
                    return Err(CoreMutationError::Plan);
                }
            }
        }
        let mut checked = capture.check(&original, &source);
        for mutation in FieldMutation::ALL {
            checked = follow(
                checked,
                capture.check_field(&original, &directory.join(mutation.label()), mutation),
                "final field mutation input check failed",
            );
        }
        checked?;
        let inventory = inventory.ok_or(CoreMutationError::Plan)?;
        validate_mutation_campaign(
            &campaign.ok_or(CoreMutationError::Plan)?,
            &inventory,
            &FieldMutation::ALL,
        )?;
        Ok(inventory)
    })();
    follow(
        operation,
        fs::remove_dir_all(directory).map_err(CoreMutationError::Io),
        "mutation temporary cleanup failed",
    )
    .map(|inventory| CheckedMutation {
        capture,
        inventory,
        image,
    })
}

pub fn execute(execution: Execution, image: &str) -> Result<(), CoreMutationError> {
    let capture = CapturedCoreMutation::read(&std::env::current_dir()?)?;
    match execution {
        Execution::DryRun => print(
            &capture,
            "preview",
            &std::env::temp_dir().join("rusterisk-core-mutation-preview"),
            image,
        ),
        Execution::Run => {
            let outcome = capture.verify(image)?;
            println!(
                "core mutation: complete native campaign and 2 field mutations accepted across {} tests",
                outcome.test_count()
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
