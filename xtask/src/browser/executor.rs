use super::{
    validate_execution, validate_inventory, BrowserError, BrowserInventory, NativeError, Phase,
    RunPlan, SourceError, SourceSnapshot,
};
pub use crate::core_proofs::Execution;
use crate::process::{run, ProcessError};
use std::{
    io::{self, Read, Write},
    path::Path,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
pub enum RunError {
    Io(io::Error),
    Plan(BrowserError),
    Source(SourceError),
    Native(NativeError),
    Process(ProcessError),
    ContainerCleanup {
        operation: String,
        cleanup: ProcessError,
    },
    DirectoryCleanup {
        operation: String,
        cleanup: io::Error,
    },
    SourceChanged {
        operation: String,
        source: SourceError,
    },
    Artifact {
        operation: String,
        artifact: io::Error,
    },
}
impl std::fmt::Display for RunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Plan(error) => error.fmt(formatter),
            Self::Source(error) => error.fmt(formatter),
            Self::Native(error) => error.fmt(formatter),
            Self::Process(error) => error.fmt(formatter),
            Self::ContainerCleanup { operation, cleanup } => write!(
                formatter,
                "{operation}; browser container cleanup failed: {cleanup}"
            ),
            Self::DirectoryCleanup { operation, cleanup } => write!(
                formatter,
                "{operation}; browser temporary directory cleanup failed: {cleanup}"
            ),
            Self::SourceChanged { operation, source } => write!(
                formatter,
                "{operation}; captured browser source check failed: {source}"
            ),
            Self::Artifact {
                operation,
                artifact,
            } => write!(
                formatter,
                "{operation}; browser artifact preservation failed: {artifact}"
            ),
        }
    }
}
impl std::error::Error for RunError {}
impl From<io::Error> for RunError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<BrowserError> for RunError {
    fn from(error: BrowserError) -> Self {
        Self::Plan(error)
    }
}
impl From<SourceError> for RunError {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}
impl From<NativeError> for RunError {
    fn from(error: NativeError) -> Self {
        Self::Native(error)
    }
}

fn read(path: &Path) -> Result<Vec<u8>, io::Error> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > 4 * 1024 * 1024 {
        return Err(io::Error::other(
            "browser native output must be a regular file within four mebibytes",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(io::Error::other(
            "browser native output exceeds four mebibytes",
        ));
    }
    Ok(bytes)
}

fn ensure_directory(path: &Path) -> Result<(), io::Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            Ok(())
        }
        Ok(_) => Err(io::Error::other(
            "browser artifact directory must be a regular directory",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => std::fs::create_dir(path),
        Err(error) => Err(error),
    }
}

fn copy(
    source: &Path,
    destination: &Path,
    count: &mut usize,
    total: &mut usize,
) -> Result<(), io::Error> {
    let bytes = read(source)?;
    *count += 1;
    *total += bytes.len();
    if *count > 64 || *total > 16 * 1024 * 1024 {
        return Err(io::Error::other("browser artifact capacity exceeded"));
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?
        .write_all(&bytes)
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    depth: usize,
    count: &mut usize,
    total: &mut usize,
) -> Result<(), io::Error> {
    if depth > 8 {
        return Err(io::Error::other("browser artifact depth exceeded"));
    }
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(io::Error::other("browser artifact symlink refused"));
        }
        if kind.is_dir() {
            copy_tree(
                &entry.path(),
                &destination.join(entry.file_name()),
                depth + 1,
                count,
                total,
            )?;
        } else {
            copy(
                &entry.path(),
                &destination.join(entry.file_name()),
                count,
                total,
            )?;
        }
    }
    Ok(())
}

fn preserve(root: &Path, state: &Path, identity: &str) -> Result<(), io::Error> {
    let cache = root.join(".cache");
    ensure_directory(&cache)?;
    let cache = cache.join("browser-runs");
    ensure_directory(&cache)?;
    let destination = cache.join(identity);
    std::fs::create_dir(&destination)?;
    let mut count = 0;
    let mut total = 0;
    for entry in std::fs::read_dir(state)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "txt" || extension == "json")
        {
            copy(
                &path,
                &destination.join(entry.file_name()),
                &mut count,
                &mut total,
            )?;
        }
    }
    let artifacts = state.join("browser-artifacts");
    match std::fs::symlink_metadata(&artifacts) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            copy_tree(
                &artifacts,
                &destination.join("screenshots"),
                0,
                &mut count,
                &mut total,
            )?
        }
        Ok(_) => {
            return Err(io::Error::other(
                "browser screenshots must be an owned directory",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    println!("browser artifacts: {}", destination.display());
    Ok(())
}

fn version(phase: Phase, bytes: &[u8]) -> Result<(), RunError> {
    let text = std::str::from_utf8(bytes).map_err(|_| NativeError)?;
    let valid = match phase {
        Phase::RustVersion => {
            text.lines()
                .filter(|line| *line == "release: 1.99.0")
                .count()
                == 1
                && text
                    .lines()
                    .filter(|line| {
                        matches!(
                            *line,
                            "host: aarch64-unknown-linux-gnu" | "host: x86_64-unknown-linux-gnu"
                        )
                    })
                    .count()
                    == 1
        }
        Phase::NodeVersion => text.trim_end() == "v26.13.1",
        Phase::BrowserVersion => text.trim_end() == "Version 1.65.1",
        Phase::Fetch
        | Phase::Install
        | Phase::Types
        | Phase::Check
        | Phase::Unit
        | Phase::Bundle
        | Phase::Compile
        | Phase::Inventory
        | Phase::Run => return Err(RunError::Native(NativeError)),
    };
    if valid {
        Ok(())
    } else {
        Err(RunError::Native(NativeError))
    }
}

pub fn execute_with(
    root: &Path,
    execution: Execution,
    image: &str,
    mut perform: impl FnMut(&mut Command, Duration) -> Result<(), ProcessError>,
) -> Result<(), RunError> {
    let snapshot = SourceSnapshot::read(root)?;
    let identity = match execution {
        Execution::DryRun => "preview".into(),
        Execution::Run => format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos()
        ),
    };
    let directory = std::env::temp_dir().join(format!("rusterisk-browser-verify-{identity}"));
    let source = directory.join("source");
    let state = directory.join("state");
    let plan = RunPlan::new(
        &identity,
        source.to_str().ok_or(BrowserError::Plan)?,
        state.to_str().ok_or(BrowserError::Plan)?,
        image,
    )?;
    println!("browser captured source: {}", snapshot.digest());
    for phase in Phase::ALL {
        let step = plan.step(phase);
        println!(
            "phase={} budget={}s docker {}",
            phase.label(),
            step.budget.as_secs(),
            step.arguments.join(" ")
        );
    }
    if matches!(execution, Execution::DryRun) {
        return Ok(());
    }
    std::fs::create_dir(&directory)?;
    let operation = (|| {
        snapshot.write(&source, &state)?;
        for name in ["cargo", "tmp", "target", "cache", "home"] {
            std::fs::create_dir(state.join(name))?;
        }
        let mut inventory: Option<BrowserInventory> = None;
        let report = state.join("browser-results.json");
        for phase in Phase::ALL {
            if phase == Phase::Run {
                std::fs::remove_file(&report)?;
            }
            let path = state.join(format!("{}.txt", phase.label()));
            let output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            let step = plan.step(phase);
            let mut command = Command::new("docker");
            command
                .args(&step.arguments)
                .current_dir(root)
                .stdout(output.try_clone()?)
                .stderr(output);
            let started = std::time::Instant::now();
            let result = perform(&mut command, step.budget);
            println!(
                "browser phase={} elapsed={:.3}s",
                phase.label(),
                started.elapsed().as_secs_f64()
            );
            if let Err(primary) = result {
                if matches!(&primary, ProcessError::Io(_) | ProcessError::Timeout) {
                    let step = plan.cleanup(phase);
                    let output = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(state.join(format!("{}-cleanup.txt", phase.label())))?;
                    let mut cleanup = Command::new("docker");
                    cleanup
                        .args(&step.arguments)
                        .current_dir(root)
                        .stdout(output.try_clone()?)
                        .stderr(output);
                    if let Err(cleanup) = perform(&mut cleanup, step.budget) {
                        return Err(RunError::ContainerCleanup {
                            operation: primary.to_string(),
                            cleanup,
                        });
                    }
                }
                return Err(RunError::Process(primary));
            }
            let bytes = read(&path)?;
            match phase {
                Phase::RustVersion | Phase::NodeVersion | Phase::BrowserVersion => {
                    version(phase, &bytes)?
                }
                Phase::Inventory => inventory = Some(validate_inventory(&read(&report)?)?),
                Phase::Run => {
                    let count = validate_execution(
                        &read(&report)?,
                        inventory.as_ref().ok_or(NativeError)?,
                    )?;
                    println!("browser: {count} inventoried native scenarios passed once");
                }
                Phase::Fetch
                | Phase::Install
                | Phase::Types
                | Phase::Check
                | Phase::Unit
                | Phase::Bundle
                | Phase::Compile => {}
            }
            snapshot.check_original(&source)?;
            snapshot.check_browser(&state)?;
        }
        Ok(())
    })();
    let checked = snapshot
        .check_original(&source)
        .and_then(|()| snapshot.check_browser(&state));
    let operation = match (operation, checked) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(RunError::Source(error)),
        (Err(operation), Err(source)) => Err(RunError::SourceChanged {
            operation: operation.to_string(),
            source,
        }),
    };
    let operation = match (operation, preserve(root, &state, &identity)) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(RunError::Io(error)),
        (Err(operation), Err(artifact)) => Err(RunError::Artifact {
            operation: operation.to_string(),
            artifact,
        }),
    };
    match (operation, std::fs::remove_dir_all(directory)) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(RunError::Io(error)),
        (Err(operation), Err(cleanup)) => Err(RunError::DirectoryCleanup {
            operation: operation.to_string(),
            cleanup,
        }),
    }
}

pub fn execute(execution: Execution, image: &str) -> Result<(), RunError> {
    execute_with(&std::env::current_dir()?, execution, image, run)
}
