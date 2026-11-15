use super::{
    validate_harnesses, validate_manifest, validate_results, CoreProofPlan, ProofError, HARNESSES,
};
use crate::process::{run, ProcessError};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy)]
pub enum Execution {
    DryRun,
    Run,
}

#[derive(PartialEq, Eq)]
struct InputFile {
    path: PathBuf,
    bytes: Vec<u8>,
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, ProofError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(ProofError::Plan);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(ProofError::Plan);
    }
    Ok(bytes)
}

fn source_files(core: &Path) -> Result<Vec<InputFile>, ProofError> {
    fn visit(
        base: &Path,
        directory: &Path,
        depth: u8,
        files: &mut Vec<InputFile>,
        bytes: &mut usize,
    ) -> Result<(), ProofError> {
        if depth > 8 {
            return Err(ProofError::Plan);
        }
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_symlink() {
                return Err(ProofError::Plan);
            }
            if kind.is_dir() {
                visit(base, &path, depth + 1, files, bytes)?;
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                if files.len() >= 64 {
                    return Err(ProofError::Plan);
                }
                let content = read_bounded(&path, 1024 * 1024)?;
                *bytes += content.len();
                if *bytes > 8 * 1024 * 1024 {
                    return Err(ProofError::Plan);
                }
                files.push(InputFile {
                    path: path
                        .strip_prefix(base)
                        .map_err(|_| ProofError::Plan)?
                        .into(),
                    bytes: content,
                });
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(core, &core.join("src"), 0, &mut files, &mut 0)?;
    let mut total: usize = files.iter().map(|file| file.bytes.len()).sum();
    for name in ["Cargo.toml", "Cargo.lock"] {
        let relative = PathBuf::from("proofs").join(name);
        let path = core.join(&relative);
        let bytes = read_bounded(&path, 16384)?;
        total += bytes.len();
        if total > 8 * 1024 * 1024 {
            return Err(ProofError::Plan);
        }
        files.push(InputFile {
            path: relative,
            bytes,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn combine<T>(
    operation: Result<T, ProofError>,
    cleanup: Result<(), ProcessError>,
) -> Result<T, ProofError> {
    match (operation, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(ProofError::Process(error)),
        (Err(operation), Err(cleanup)) => Err(ProofError::Cleanup {
            operation: operation.to_string(),
            cleanup,
        }),
    }
}

pub fn execute(execution: Execution, image: &str) -> Result<(), ProofError> {
    let project = std::env::current_dir()?;
    let core = project.join("crates/core");
    let files = source_files(&core)?;
    let manifest = files
        .iter()
        .find(|file| file.path == Path::new("proofs/Cargo.toml"))
        .ok_or(ProofError::Plan)?;
    validate_manifest(std::str::from_utf8(&manifest.bytes).map_err(|_| ProofError::Plan)?)?;
    let harnesses = files
        .iter()
        .find(|file| file.path == Path::new("src/proofs.rs"))
        .ok_or(ProofError::Harnesses)?;
    validate_harnesses(std::str::from_utf8(&harnesses.bytes).map_err(|_| ProofError::Harnesses)?)?;
    let identity = match execution {
        Execution::DryRun => "preview".to_owned(),
        Execution::Run => format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| ProofError::Plan)?
                .as_nanos()
        ),
    };
    let directory = std::env::temp_dir().join(format!("rusterisk-core-proofs-{identity}"));
    let source = directory.join("source");
    let results = directory.join("results");
    let plan = CoreProofPlan::new(
        &identity,
        source.to_str().ok_or(ProofError::Plan)?,
        results.to_str().ok_or(ProofError::Plan)?,
        image,
    )?;
    let step = plan.run();
    println!("Kani 0.68.0: {} harnesses; snapshot the production core; temporary source <= 8 MiB; native export <= 128 MiB; accepted report <= 4 MiB", HARNESSES.len());
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
    if matches!(execution, Execution::DryRun) {
        return Ok(());
    }
    std::fs::create_dir(&directory)?;
    let operation = (|| {
        std::fs::create_dir(&source)?;
        std::fs::create_dir(&results)?;
        for file in &files {
            let destination = source.join(&file.path);
            std::fs::create_dir_all(destination.parent().ok_or(ProofError::Plan)?)?;
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)?;
            std::io::Write::write_all(&mut output, &file.bytes)?;
        }
        let outcome = run(Command::new("docker").args(&step.arguments), step.budget);
        let interrupted = matches!(&outcome, Err(ProcessError::Io(_) | ProcessError::Timeout));
        let cleanup = if interrupted {
            run(
                Command::new("docker").args(&cleanup.arguments),
                cleanup.budget,
            )
        } else {
            Ok(())
        };
        combine(outcome.map_err(ProofError::Process), cleanup)?;
        if source_files(&core)? != files {
            return Err(ProofError::Plan);
        }
        let mut bytes = Vec::new();
        std::fs::File::open(results.join("kani.json"))?
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        validate_results(&bytes)?;
        println!(
            "core proofs: {} Kani harnesses passed with reachable contracts",
            HARNESSES.len()
        );
        Ok(())
    })();
    combine(
        operation,
        std::fs::remove_dir_all(directory).map_err(ProcessError::Io),
    )
}
