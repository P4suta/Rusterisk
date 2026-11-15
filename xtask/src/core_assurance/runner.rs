use super::{
    snapshot::{read_bounded, Snapshot},
    validate_miri_discovery, validate_miri_execution, validate_mutation_inventory,
    validate_mutation_outcomes, AssuranceError, CoreAssurancePlan, Execution, Phase,
};
use crate::process::{run, ProcessError};
use std::{path::Path, process::Command};

#[derive(Clone, Copy)]
pub enum Task {
    Miri,
    Mutation,
}

fn combine<T>(
    operation: Result<T, AssuranceError>,
    cleanup: Result<(), ProcessError>,
) -> Result<T, AssuranceError> {
    match (operation, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(AssuranceError::Process(error)),
        (Err(operation), Err(cleanup)) => Err(AssuranceError::Cleanup {
            operation: operation.to_string(),
            cleanup,
        }),
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), AssuranceError> {
    std::fs::create_dir_all(path.parent().ok_or(AssuranceError::Plan)?)?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    std::io::Write::write_all(&mut output, bytes)?;
    Ok(())
}

pub fn execute(task: Task, execution: Execution, image: &str) -> Result<(), AssuranceError> {
    let core = std::env::current_dir()?.join("crates/core");
    let snapshot = Snapshot::read(&core)?;
    let identity = match execution {
        Execution::DryRun => "preview".to_owned(),
        Execution::Run => format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| AssuranceError::Plan)?
                .as_nanos()
        ),
    };
    let directory = std::env::temp_dir().join(format!("rusterisk-core-assurance-{identity}"));
    let source = directory.join("source");
    let state = directory.join("state");
    let plan = CoreAssurancePlan::new(
        &identity,
        source.to_str().ok_or(AssuranceError::Plan)?,
        state.to_str().ok_or(AssuranceError::Plan)?,
        image,
    )?;
    let phases: &[Phase] = match task {
        Task::Miri => &[Phase::MiriSetup, Phase::MiriInventory, Phase::MiriTests],
        Task::Mutation => &[Phase::MutationInventory, Phase::Mutations],
    };
    println!("core assurance: {} integration targets; {} tests; source snapshot <= 8 MiB; native reports <= 4 MiB", snapshot.targets.len(), snapshot.tests.len());
    for phase in phases {
        let step = plan.step(*phase);
        println!(
            "phase={} budget={}s docker {}",
            phase.label(),
            step.budget.as_secs(),
            step.arguments.join(" ")
        );
        let cleanup = plan.cleanup(*phase);
        println!(
            "interruption cleanup: budget={}s docker {}",
            cleanup.budget.as_secs(),
            cleanup.arguments.join(" ")
        );
    }
    if matches!(execution, Execution::DryRun) {
        return Ok(());
    }
    std::fs::create_dir(&directory)?;
    let operation = (|| {
        std::fs::create_dir(&source)?;
        for name in ["cargo", "tmp", "results"] {
            std::fs::create_dir_all(state.join(name))?;
        }
        for file in &snapshot.files {
            if file.path.starts_with("src") || file.path.starts_with("tests") {
                write_new(&source.join(&file.path), &file.bytes)?;
            } else if file.path == Path::new("assurance/Cargo.lock") {
                write_new(&source.join("Cargo.lock"), &file.bytes)?;
            }
        }
        write_new(&source.join("Cargo.toml"), snapshot.normalized.as_bytes())?;
        let mut miri = None;
        let mut mutations = None;
        for phase in phases {
            let step = plan.step(*phase);
            let output = state.join(format!("{}.txt", phase.label()));
            let capture = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&output)?;
            let mut command = Command::new("docker");
            command.args(&step.arguments).stdout(capture);
            let started = std::time::Instant::now();
            let outcome = run(&mut command, step.budget);
            println!(
                "phase={} elapsed={:.3}s",
                phase.label(),
                started.elapsed().as_secs_f64()
            );
            let interrupted = matches!(&outcome, Err(ProcessError::Io(_) | ProcessError::Timeout));
            let cleanup = if interrupted {
                let step = plan.cleanup(*phase);
                run(Command::new("docker").args(&step.arguments), step.budget)
            } else {
                Ok(())
            };
            let checked = combine(outcome.map_err(AssuranceError::Process), cleanup);
            let capture = read_bounded(&output, 4 * 1024 * 1024);
            if let Err(error) = checked {
                match capture {
                    Ok(bytes) => eprintln!("{}", String::from_utf8_lossy(&bytes)),
                    Err(read_error) => {
                        eprintln!("assurance output could not be read: {read_error}")
                    }
                }
                return Err(error);
            }
            let bytes = capture?;
            match phase {
                Phase::MiriSetup => println!("{}", String::from_utf8_lossy(&bytes)),
                Phase::MiriInventory => {
                    miri = Some(validate_miri_discovery(
                        &bytes,
                        &snapshot.tests,
                        snapshot.targets.len(),
                    )?)
                }
                Phase::MiriTests => validate_miri_execution(
                    &bytes,
                    miri.as_ref().ok_or(AssuranceError::Inventory)?,
                )?,
                Phase::MutationInventory => mutations = Some(validate_mutation_inventory(&bytes)?),
                Phase::Mutations => {
                    let bytes = read_bounded(
                        &state.join("results/mutants.out/outcomes.json"),
                        4 * 1024 * 1024,
                    )?;
                    let result = validate_mutation_outcomes(
                        &bytes,
                        mutations.as_ref().ok_or(AssuranceError::Inventory)?,
                    );
                    if result.is_err() {
                        let capture = read_bounded(&state.join("mutations.txt"), 4 * 1024 * 1024)?;
                        eprintln!("{}", String::from_utf8_lossy(&capture));
                    }
                    result?;
                }
            }
        }
        if Snapshot::read(&core)?.files != snapshot.files {
            return Err(AssuranceError::Plan);
        }
        match task {
            Task::Miri => println!(
                "core Miri: {} tests passed without skipped tests",
                snapshot.tests.len()
            ),
            Task::Mutation => println!("core mutation: complete native outcomes accepted"),
        }
        Ok(())
    })();
    combine(
        operation,
        std::fs::remove_dir_all(directory).map_err(ProcessError::Io),
    )
}
