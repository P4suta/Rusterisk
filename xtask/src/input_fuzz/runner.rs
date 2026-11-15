use super::{
    snapshot::{read_bounded, write_new, Snapshot},
    validate_inventory, validate_run, Execution, FuzzError, FuzzPlan, LinuxTarget, Phase, Target,
    MAX_INPUT,
};
use crate::process::{run, ProcessError};
use std::{path::Path, process::Command};

#[derive(Clone, Copy)]
pub enum Scope {
    All,
    Target(Target),
}

impl Scope {
    pub fn parse(value: &str) -> Result<Self, FuzzError> {
        if value == "all" {
            return Ok(Self::All);
        }
        Target::ALL
            .into_iter()
            .find(|target| target.name() == value)
            .map(Self::Target)
            .ok_or(FuzzError::Plan)
    }
    fn targets(self) -> Vec<Target> {
        match self {
            Self::All => Target::ALL.into(),
            Self::Target(target) => [target].into(),
        }
    }
}

fn combine<T>(
    operation: Result<T, FuzzError>,
    cleanup: Result<(), ProcessError>,
) -> Result<T, FuzzError> {
    match (operation, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(FuzzError::Process(error)),
        (Err(operation), Err(cleanup)) => Err(FuzzError::Cleanup {
            operation: operation.to_string(),
            cleanup,
        }),
    }
}

fn preserve(root: &Path, state: &Path, identity: &str, target: Target) -> Result<(), FuzzError> {
    let directory = state.join("artifacts").join(target.name());
    let mut count = 0;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        count += 1;
        let name = entry.file_name();
        let name = name.to_str().ok_or(FuzzError::Results)?;
        if count > 16
            || name.len() > 160
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(FuzzError::Results);
        }
        let bytes = read_bounded(&entry.path(), MAX_INPUT)?;
        let path = root
            .join(".cache/input-fuzz-crashes")
            .join(identity)
            .join(target.name())
            .join(name);
        write_new(&path, &bytes)?;
        eprintln!("fuzz failing input preserved at {}", path.display());
    }
    Ok(())
}

pub fn execute(scope: Scope, execution: Execution, image: &str) -> Result<(), FuzzError> {
    let root = std::env::current_dir()?;
    let snapshot = Snapshot::read(&root)?;
    let identity = match execution {
        Execution::DryRun => "preview".to_owned(),
        Execution::Run => format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| FuzzError::Plan)?
                .as_nanos()
        ),
    };
    let directory = std::env::temp_dir().join(format!("rusterisk-input-fuzz-{identity}"));
    let source = directory.join("source");
    let state = directory.join("state");
    let architecture = LinuxTarget::host()?;
    let plan = FuzzPlan::new(
        &identity,
        source.to_str().ok_or(FuzzError::Plan)?,
        state.to_str().ok_or(FuzzError::Plan)?,
        image,
        architecture,
    )?;
    let targets = scope.targets();
    let mut phases: Vec<Phase> = [
        Phase::RustVersion,
        Phase::Version,
        Phase::Inventory,
        Phase::Fetch,
    ]
    .into_iter()
    .collect();
    for target in &targets {
        let seeds = target.seeds();
        if seeds.is_empty()
            || !seeds.iter().any(|seed| seed.valid)
            || !seeds.iter().any(|seed| !seed.valid)
        {
            return Err(FuzzError::Inventory);
        }
        phases.extend([Phase::Build(*target), Phase::Run(*target)]);
    }
    println!("input fuzz: {} targets; snapshot <= 16 MiB; native output <= 4 MiB; explicit production path dependencies", targets.len());
    println!("{}", snapshot.normalized);
    for phase in &phases {
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
        for name in ["cargo", "tmp", "target", "artifacts", "corpus"] {
            std::fs::create_dir_all(state.join(name))?;
        }
        snapshot.write(&source, &state)?;
        for target in &targets {
            std::fs::create_dir_all(state.join("artifacts").join(target.name()))?;
            for seed in target.seeds() {
                if seed.bytes.is_empty() || seed.bytes.len() > MAX_INPUT {
                    return Err(FuzzError::Inventory);
                }
                write_new(
                    &state.join("corpus").join(target.name()).join(seed.name),
                    seed.bytes,
                )?;
            }
        }
        for phase in phases {
            let step = plan.step(phase);
            let path = state.join(format!("{}.txt", phase.label()));
            let capture = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            let mut command = Command::new("docker");
            command
                .args(&step.arguments)
                .stdout(capture.try_clone()?)
                .stderr(capture);
            let started = std::time::Instant::now();
            let result = run(&mut command, step.budget);
            println!(
                "phase={} elapsed={:.3}s",
                phase.label(),
                started.elapsed().as_secs_f64()
            );
            let cleanup = if matches!(&result, Err(ProcessError::Io(_) | ProcessError::Timeout)) {
                let cleanup = plan.cleanup(phase);
                run(
                    Command::new("docker").args(&cleanup.arguments),
                    cleanup.budget,
                )
            } else {
                Ok(())
            };
            if let Err(error) = combine(result.map_err(FuzzError::Process), cleanup) {
                match read_bounded(&path, 4 * 1024 * 1024) {
                    Ok(bytes) => eprintln!("{}", String::from_utf8_lossy(&bytes)),
                    Err(error) => eprintln!("fuzz output could not be read: {error}"),
                }
                if let Phase::Run(target) = phase {
                    return combine(
                        Err(error),
                        preserve(&root, &state, &identity, target).map_err(|error| {
                            ProcessError::Io(std::io::Error::other(error.to_string()))
                        }),
                    );
                }
                return Err(error);
            }
            let bytes = read_bounded(&path, 4 * 1024 * 1024)?;
            match phase {
                Phase::RustVersion => {
                    let text = std::str::from_utf8(&bytes).map_err(|_| FuzzError::Results)?;
                    if !text
                        .lines()
                        .any(|line| line == "commit-hash: 8925ea358a0f265ca61026aadc7ecc506c545cbe")
                        || !text
                            .lines()
                            .any(|line| line == format!("host: {}", architecture.triple()))
                    {
                        return Err(FuzzError::Results);
                    }
                }
                Phase::Version => {
                    if bytes.strip_suffix(b"\n").unwrap_or(&bytes) != b"cargo-fuzz 0.13.2" {
                        return Err(FuzzError::Results);
                    }
                }
                Phase::Inventory => validate_inventory(&bytes)?,
                Phase::Fetch | Phase::Build(_) => snapshot.check_harness(&state)?,
                Phase::Run(target) => {
                    let count = validate_run(&bytes, target.seeds().len())?;
                    if std::fs::read_dir(state.join("artifacts").join(target.name()))?
                        .next()
                        .is_some()
                    {
                        preserve(&root, &state, &identity, target)?;
                        return Err(FuzzError::Results);
                    }
                    println!("input fuzz: {} completed {count} executions", target.name());
                }
            }
        }
        if Snapshot::read(&root)?.files != snapshot.files {
            return Err(FuzzError::Manifest);
        }
        snapshot.check_harness(&state)?;
        println!("input fuzz: every selected target completed without crashes");
        Ok(())
    })();
    combine(
        operation,
        std::fs::remove_dir_all(directory).map_err(ProcessError::Io),
    )
}
