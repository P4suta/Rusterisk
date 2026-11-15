use super::{
    runner::write_new,
    snapshot::{read_bounded, Snapshot},
    validate_field_execution, AssuranceError, CoreAssurancePlan, FieldMutation, Phase,
};
use crate::process::{run, ProcessError};
use std::{path::Path, process::Command};

pub(super) fn preview(
    plan: &CoreAssurancePlan,
    directory: &Path,
    state: &Path,
) -> Result<(), AssuranceError> {
    println!("mutation supplements: 2 source-bound field mutations; native build success and complete test failures required");
    for mutation in FieldMutation::ALL {
        let source = directory.join(mutation.label());
        let plan = CoreAssurancePlan::new(
            &plan.identity,
            source.to_str().ok_or(AssuranceError::Plan)?,
            state.to_str().ok_or(AssuranceError::Plan)?,
            plan.image.as_str(),
        )?;
        for phase in [Phase::FieldBuild(mutation), Phase::FieldTests(mutation)] {
            let step = plan.step(phase);
            println!(
                "phase={} budget={}s docker {}",
                phase.label(),
                step.budget.as_secs(),
                step.arguments.join(" ")
            );
            let step = plan.cleanup(phase);
            println!(
                "interruption cleanup: budget={}s docker {}",
                step.budget.as_secs(),
                step.arguments.join(" ")
            );
        }
    }
    Ok(())
}

pub(super) fn execute(
    snapshot: &Snapshot,
    plan: &CoreAssurancePlan,
    directory: &Path,
    state: &Path,
) -> Result<(), AssuranceError> {
    for mutation in FieldMutation::ALL {
        let source = directory.join(mutation.label());
        std::fs::create_dir(&source)?;
        for file in &snapshot.files {
            if file.path.starts_with("src") || file.path.starts_with("tests") {
                if file.path == Path::new("src/lib.rs") {
                    let original =
                        std::str::from_utf8(&file.bytes).map_err(|_| AssuranceError::Plan)?;
                    write_new(
                        &source.join(&file.path),
                        mutation.apply(original)?.as_bytes(),
                    )?;
                } else {
                    write_new(&source.join(&file.path), &file.bytes)?;
                }
            } else if file.path == Path::new("assurance/Cargo.lock") {
                write_new(&source.join("Cargo.lock"), &file.bytes)?;
            }
        }
        write_new(&source.join("Cargo.toml"), snapshot.normalized.as_bytes())?;
        let field_plan = CoreAssurancePlan::new(
            &plan.identity,
            source.to_str().ok_or(AssuranceError::Plan)?,
            state.to_str().ok_or(AssuranceError::Plan)?,
            plan.image.as_str(),
        )?;
        for phase in [Phase::FieldBuild(mutation), Phase::FieldTests(mutation)] {
            let step = field_plan.step(phase);
            let path = state.join(format!("{}.txt", phase.label()));
            let capture = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            let started = std::time::Instant::now();
            let result = run(
                Command::new("docker").args(&step.arguments).stdout(capture),
                step.budget,
            );
            println!(
                "phase={} elapsed={:.3}s",
                phase.label(),
                started.elapsed().as_secs_f64()
            );
            if matches!(&result, Err(ProcessError::Io(_) | ProcessError::Timeout)) {
                let cleanup = field_plan.cleanup(phase);
                let cleanup = run(
                    Command::new("docker").args(&cleanup.arguments),
                    cleanup.budget,
                );
                if let Err(cleanup) = cleanup {
                    return Err(AssuranceError::Cleanup {
                        operation: result.err().ok_or(AssuranceError::Plan)?.to_string(),
                        cleanup,
                    });
                }
            }
            match phase {
                Phase::FieldBuild(_) => result?,
                Phase::FieldTests(_) => {
                    if !matches!(&result, Err(ProcessError::Status(status)) if status.code() == Some(101))
                    {
                        return Err(match result {
                            Err(error) => AssuranceError::Process(error),
                            Ok(()) => AssuranceError::Results,
                        });
                    }
                    let bytes = read_bounded(&path, 4 * 1024 * 1024)?;
                    validate_field_execution(&bytes, &snapshot.tests, snapshot.targets.len())?;
                    println!("field mutation caught: {}", mutation.label());
                }
                Phase::MiriSetup
                | Phase::MiriInventory
                | Phase::MiriTests
                | Phase::MutationInventory
                | Phase::Mutations => return Err(AssuranceError::Plan),
            }
        }
    }
    Ok(())
}
