use super::{
    validate_execution, validate_inventory, BrowserError, BrowserInventory, NativeError, Phase,
    RunPlan, SourceError, SourceSnapshot,
};
pub use crate::core_proofs::Execution;
use crate::process::{run, CommandSupervisor, ProcessError};
use crate::reference::Step;
#[cfg(unix)]
use crate::reference::{
    with_browser_package, with_browser_reference, with_measured_browser_reference, ReferenceImage,
};
#[cfg(unix)]
use crate::server_distribution::{ServerCandidate, ServerTarget};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
mod retained;
#[cfg(unix)]
pub(crate) use retained::{read as read_retained, run as run_retained, BrowserExecution};
#[cfg(test)]
mod tests;

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
    SupervisorCleanup {
        operation: String,
        cleanup: ProcessError,
    },
    Retained {
        preceding: Option<String>,
        stage: &'static str,
        failure: io::Error,
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
            Self::SupervisorCleanup { operation, cleanup } => write!(
                formatter,
                "{operation}; browser process supervisor cleanup failed: {cleanup}"
            ),
            Self::Retained {
                preceding,
                stage,
                failure,
            } => {
                if let Some(preceding) = preceding {
                    write!(formatter, "{preceding}; ")?;
                }
                write!(formatter, "{stage}: {failure}")
            }
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

fn preserve(root: &Path, state: &Path, identity: &str) -> Result<PathBuf, io::Error> {
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
    for (name, output) in [
        ("browser-artifacts", "screenshots"),
        ("reference-browser-artifacts", "reference-screenshots"),
    ] {
        let artifacts = state.join(name);
        match std::fs::symlink_metadata(&artifacts) {
            Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
                copy_tree(
                    &artifacts,
                    &destination.join(output),
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
    }
    println!("browser artifacts: {}", destination.display());
    Ok(destination)
}

fn version(phase: Phase, bytes: &[u8]) -> Result<(), RunError> {
    let text = std::str::from_utf8(bytes).map_err(|_| NativeError)?;
    let valid = match phase {
        Phase::RustVersion | Phase::CargoVersion => {
            text.lines()
                .filter(|line| *line == "release: 1.100.0")
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
        Phase::NpmVersion => text.trim_end() == "11.20.0",
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

pub struct PreparedBrowserRun {
    root: PathBuf,
    snapshot: SourceSnapshot,
    identity: String,
    directory: PathBuf,
    plan: RunPlan,
    #[cfg(unix)]
    reference: Option<ReferenceRun>,
}

#[cfg(unix)]
struct ReferenceRun {
    image: ReferenceImage,
    package: Option<PackagedComparison>,
}

#[cfg(unix)]
struct PackagedComparison {
    candidate: ServerCandidate,
    order: PackagedOrder,
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PackagedOrder {
    RusteriskFirst,
    AsteriskFirst,
}

#[cfg(unix)]
impl PackagedOrder {
    const fn label(self) -> &'static str {
        match self {
            Self::RusteriskFirst => "rusterisk,asterisk",
            Self::AsteriskFirst => "asterisk,rusterisk",
        }
    }
}

/// Native completion requires successful validation and cleanup.
///
/// ```compile_fail
/// use std::path::PathBuf;
/// use xtask::browser::{BrowserInventory, BrowserOutcome, RunPlan, SourceSnapshot};
/// fn forge(snapshot: SourceSnapshot, plan: RunPlan, inventory: BrowserInventory, artifacts: PathBuf) -> BrowserOutcome {
///     BrowserOutcome { snapshot, plan, inventory, artifacts }
/// }
/// ```
pub struct BrowserOutcome {
    snapshot: SourceSnapshot,
    plan: RunPlan,
    inventory: BrowserInventory,
    artifacts: PathBuf,
    reference: Option<String>,
}

impl BrowserOutcome {
    pub fn inputs(&self) -> impl ExactSizeIterator<Item = (&Path, &[u8])> {
        self.snapshot.files()
    }

    pub fn image(&self) -> &str {
        self.plan.image()
    }

    pub fn reference_image(&self) -> Option<&str> {
        self.reference.as_deref()
    }

    pub const fn inventory(&self) -> &BrowserInventory {
        &self.inventory
    }

    pub fn artifacts(&self) -> &Path {
        &self.artifacts
    }
}

impl PreparedBrowserRun {
    pub fn read(root: &Path, image: &str) -> Result<Self, RunError> {
        let snapshot = SourceSnapshot::read(root)?;
        let identity = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos()
        );
        let directory = std::env::temp_dir().join(format!("rusterisk-browser-verify-{identity}"));
        let source = directory.join("source");
        let state = directory.join("state");
        let plan = RunPlan::new(
            &identity,
            source.to_str().ok_or(BrowserError::Plan)?,
            state.to_str().ok_or(BrowserError::Plan)?,
            image,
        )?;
        Ok(Self {
            root: root.to_owned(),
            snapshot,
            identity,
            directory,
            plan,
            #[cfg(unix)]
            reference: None,
        })
    }

    pub const fn plan(&self) -> &RunPlan {
        &self.plan
    }

    #[cfg(unix)]
    pub(crate) fn with_reference(self, reference: &str) -> Result<Self, RunError> {
        self.with_comparison(reference, None)
    }

    #[cfg(unix)]
    fn with_comparison(
        mut self,
        reference: &str,
        package: Option<PackagedComparison>,
    ) -> Result<Self, RunError> {
        self.reference = Some(ReferenceRun {
            image: ReferenceImage::parse_inspection(reference).map_err(io::Error::other)?,
            package,
        });
        Ok(self)
    }

    pub fn inputs(&self) -> impl ExactSizeIterator<Item = (&Path, &[u8])> {
        self.snapshot.files()
    }

    fn reserve(mut self) -> Result<Self, RunError> {
        let parent = self
            .directory
            .parent()
            .ok_or(BrowserError::Plan)?
            .to_owned();
        for attempt in 0..32 {
            let (identity, directory) = if attempt == 0 {
                (self.identity.clone(), self.directory.clone())
            } else {
                let identity = format!("{}-{attempt}", self.identity);
                let directory = parent.join(format!("rusterisk-browser-verify-{identity}"));
                (identity, directory)
            };
            let plan = RunPlan::new(
                &identity,
                directory
                    .join("source")
                    .to_str()
                    .ok_or(BrowserError::Plan)?,
                directory.join("state").to_str().ok_or(BrowserError::Plan)?,
                self.plan.image(),
            )?;
            match std::fs::create_dir(&directory) {
                Ok(()) => {
                    self.identity = identity;
                    self.directory = directory;
                    self.plan = plan;
                    return Ok(self);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(RunError::Io(error)),
            }
        }
        Err(RunError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "browser working directory reservation limit exceeded",
        )))
    }

    fn print(&self) {
        println!("browser captured source: {}", self.snapshot.digest());
        for phase in Phase::ALL {
            #[cfg(unix)]
            if phase == Phase::Run
                && self
                    .reference
                    .as_ref()
                    .is_some_and(|reference| reference.package.is_some())
            {
                println!("phase=run against a packaged server in its isolated bridge namespace");
                continue;
            }
            let step = self.plan.step(phase);
            println!(
                "phase={} budget={}s docker {}",
                phase.label(),
                step.budget.as_secs(),
                step.arguments.join(" ")
            );
        }
        #[cfg(unix)]
        if let Some(reference) = &self.reference {
            println!(
                "browser reference image={} isolated bridge, browser and reference combined CPU=2 memory=2GiB",
                reference.image.as_str()
            );
            if let Some(package) = &reference.package {
                println!(
                    "packaged browser server source={} target={} sha256={} order={}",
                    package.candidate.source_digest(),
                    package.candidate.name().target().triple(),
                    package.candidate.checksum(),
                    package.order.label()
                );
            }
        }
    }

    pub fn run(self) -> Result<BrowserOutcome, RunError> {
        let mut supervisor = CommandSupervisor::new().map_err(RunError::Process)?;
        let operation = run_prepared_with(self, |command, budget, mode| match mode {
            BrowserCommand::Run => supervisor.run(command, budget),
            BrowserCommand::Cleanup => supervisor.run_cleanup(command, budget),
        });
        finish(operation, supervisor.finish())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BrowserCommand {
    Run,
    Cleanup,
}

struct CheckedBrowserRun {
    snapshot: SourceSnapshot,
    plan: RunPlan,
    inventory: BrowserInventory,
    artifacts: PathBuf,
    reference: Option<String>,
}

fn finish(
    operation: Result<CheckedBrowserRun, RunError>,
    cleanup: Result<(), ProcessError>,
) -> Result<BrowserOutcome, RunError> {
    match (operation, cleanup) {
        (Ok(checked), Ok(())) => Ok(BrowserOutcome {
            snapshot: checked.snapshot,
            plan: checked.plan,
            inventory: checked.inventory,
            artifacts: checked.artifacts,
            reference: checked.reference,
        }),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(RunError::Process(error)),
        (Err(operation), Err(cleanup)) => Err(RunError::SupervisorCleanup {
            operation: operation.to_string(),
            cleanup,
        }),
    }
}

fn run_phase(
    root: &Path,
    state: &Path,
    label: &str,
    step: Step,
    cleanup: Step,
    perform: &mut impl FnMut(&mut Command, Duration, BrowserCommand) -> Result<(), ProcessError>,
) -> Result<Vec<u8>, RunError> {
    let path = state.join(format!("{label}.txt"));
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let mut command = Command::new("docker");
    command
        .args(&step.arguments)
        .current_dir(root)
        .stdout(output.try_clone()?)
        .stderr(output);
    let started = std::time::Instant::now();
    let result = perform(&mut command, step.budget, BrowserCommand::Run);
    println!(
        "browser phase={label} elapsed={:.3}s",
        started.elapsed().as_secs_f64()
    );
    if let Err(primary) = result {
        if matches!(
            &primary,
            ProcessError::Io(_)
                | ProcessError::Timeout
                | ProcessError::Interrupted
                | ProcessError::Cleanup { .. }
        ) {
            let capture = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(state.join(format!("{label}-cleanup.txt")))
                .and_then(|stderr| stderr.try_clone().map(|stdout| (stdout, stderr)));
            let mut command = Command::new("docker");
            command.args(&cleanup.arguments).current_dir(root);
            let capture_error = match capture {
                Ok((stdout, stderr)) => {
                    command.stdout(stdout).stderr(stderr);
                    None
                }
                Err(error) => {
                    command.stdout(Stdio::null()).stderr(Stdio::null());
                    Some(error)
                }
            };
            let cleaned = match (
                perform(&mut command, cleanup.budget, BrowserCommand::Cleanup),
                capture_error,
            ) {
                (result, None) => result,
                (Ok(()), Some(error)) => Err(ProcessError::Io(error)),
                (Err(cleanup), Some(operation)) => Err(ProcessError::Cleanup {
                    operation: operation.to_string(),
                    cleanup: cleanup.to_string(),
                }),
            };
            if let Err(cleanup) = cleaned {
                return Err(RunError::ContainerCleanup {
                    operation: primary.to_string(),
                    cleanup,
                });
            }
        }
        return Err(RunError::Process(primary));
    }
    read(&path).map_err(RunError::Io)
}

fn run_prepared_with<F>(
    prepared: PreparedBrowserRun,
    mut perform: F,
) -> Result<CheckedBrowserRun, RunError>
where
    F: FnMut(&mut Command, Duration, BrowserCommand) -> Result<(), ProcessError>,
{
    let prepared = prepared.reserve()?;
    prepared.print();
    let PreparedBrowserRun {
        root,
        snapshot,
        identity,
        directory,
        plan,
        #[cfg(unix)]
        reference,
    } = prepared;
    #[cfg(unix)]
    let reference_image = reference
        .as_ref()
        .map(|reference| reference.image.as_str().to_owned());
    #[cfg(unix)]
    let packaged = reference.as_ref().and_then(|reference| {
        reference
            .package
            .as_ref()
            .map(|package| (&reference.image, package))
    });
    #[cfg(unix)]
    let (early_reference, late_reference) = match reference.as_ref() {
        Some(reference)
            if packaged
                .is_some_and(|(_, package)| package.order == PackagedOrder::AsteriskFirst) =>
        {
            (Some(reference), None)
        }
        reference => (None, reference),
    };
    #[cfg(not(unix))]
    let reference_image = None;
    let source = directory.join("source");
    let state = directory.join("state");
    let operation = (|| {
        snapshot.write(&source, &state)?;
        for name in ["cargo", "tmp", "target", "cache", "home"] {
            std::fs::create_dir(state.join(name))?;
        }
        let mut inventory: Option<BrowserInventory> = None;
        let report = state.join("browser-results.json");
        #[cfg(unix)]
        if let Some((_, package)) = packaged {
            let mut binding = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(state.join("packaged-server.txt"))?;
            writeln!(
                binding,
                "source={}\ntarget={}\nsha256={}\norder={}",
                package.candidate.source_digest(),
                package.candidate.name().target().triple(),
                package.candidate.checksum(),
                package.order.label()
            )?;
            binding.flush()?;
        }
        #[cfg(unix)]
        let run_reference = |reference: &ReferenceRun,
                             inventory: &BrowserInventory,
                             perform: &mut F|
         -> Result<(), RunError> {
            let image = &reference.image;
            let mut profile = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(state.join("reference-images.txt"))?;
            writeln!(
                profile,
                "source={}\nbrowser={}\nasterisk={}",
                snapshot.digest(),
                plan.image(),
                image.as_str()
            )?;
            profile.flush()?;
            let mut compare = |id: &str| {
                let step = match &reference.package {
                    Some(_) => plan.matched_reference_step(id),
                    None => plan.reference_step(id),
                }
                .map_err(io::Error::other)?;
                run_phase(
                    &root,
                    &state,
                    "reference-run",
                    step,
                    plan.cleanup(Phase::Run),
                    perform,
                )
                .map_err(io::Error::other)
            };
            let validate = || {
                super::runtime_inputs::RuntimeInputs::read(
                    &state.join("reference-browser-artifacts/runtime"),
                )?;
                validate_execution(
                    &read(&state.join("reference-browser-results.json"))?,
                    inventory,
                )
                .map_err(io::Error::other)
            };
            let count = match &reference.package {
                Some(_) => {
                    with_measured_browser_reference(&source, &state, &identity, image, compare)?;
                    validate()?
                }
                None => with_browser_reference(&source, &state, &identity, image, |id| {
                    compare(id)?;
                    validate()
                })?,
            };
            println!("browser: {count} inventoried Asterisk scenarios passed once");
            Ok(())
        };
        for phase in Phase::ALL {
            if phase == Phase::Run {
                #[cfg(unix)]
                if let Some(reference) = early_reference {
                    run_reference(
                        reference,
                        inventory.as_ref().ok_or(NativeError)?,
                        &mut perform,
                    )?;
                }
                std::fs::remove_file(&report)?;
            }
            let mut run = |step| {
                run_phase(
                    &root,
                    &state,
                    phase.label(),
                    step,
                    plan.cleanup(phase),
                    &mut perform,
                )
            };
            #[cfg(unix)]
            let bytes = match (phase, packaged) {
                (Phase::Run, Some((image, package))) => with_browser_package(
                    &source,
                    &state,
                    &identity,
                    image,
                    &package.candidate,
                    |id| {
                        let step = plan.packaged_step(id).map_err(io::Error::other)?;
                        run(step).map_err(io::Error::other)
                    },
                )?,
                _ => run(plan.step(phase))?,
            };
            #[cfg(not(unix))]
            let bytes = run(plan.step(phase))?;
            match phase {
                Phase::RustVersion
                | Phase::CargoVersion
                | Phase::NodeVersion
                | Phase::NpmVersion
                | Phase::BrowserVersion => version(phase, &bytes)?,
                Phase::Inventory => {
                    let bytes = read(&report)?;
                    inventory = Some(validate_inventory(&bytes)?);
                    let mut saved = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(state.join("browser-inventory.json"))?;
                    saved.write_all(&bytes)?;
                    saved.flush()?;
                }
                Phase::Run => {
                    #[cfg(unix)]
                    if packaged.is_some() {
                        super::runtime_inputs::RuntimeInputs::read(
                            &state.join("browser-artifacts/runtime"),
                        )?;
                    }
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
        let inventory = inventory.ok_or(RunError::Native(NativeError))?;
        #[cfg(unix)]
        if let Some(reference) = late_reference {
            run_reference(reference, &inventory, &mut perform)?;
        }
        Ok(inventory)
    })();
    let checked = snapshot
        .check_original(&source)
        .and_then(|()| snapshot.check_browser(&state));
    let operation = match (operation, checked) {
        (Ok(inventory), Ok(())) => Ok(inventory),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(RunError::Source(error)),
        (Err(operation), Err(source)) => Err(RunError::SourceChanged {
            operation: operation.to_string(),
            source,
        }),
    };
    let operation = match (operation, preserve(&root, &state, &identity)) {
        (Ok(inventory), Ok(artifacts)) => Ok((inventory, artifacts)),
        (Err(error), Ok(_)) => Err(error),
        (Ok(_), Err(error)) => Err(RunError::Io(error)),
        (Err(operation), Err(artifact)) => Err(RunError::Artifact {
            operation: operation.to_string(),
            artifact,
        }),
    };
    match (operation, std::fs::remove_dir_all(directory)) {
        (Ok((inventory, artifacts)), Ok(())) => Ok(CheckedBrowserRun {
            snapshot,
            plan,
            inventory,
            artifacts,
            reference: reference_image,
        }),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(RunError::Io(error)),
        (Err(operation), Err(cleanup)) => Err(RunError::DirectoryCleanup {
            operation: operation.to_string(),
            cleanup,
        }),
    }
}

pub fn execute_with(
    root: &Path,
    execution: Execution,
    image: &str,
    mut perform: impl FnMut(&mut Command, Duration) -> Result<(), ProcessError>,
) -> Result<(), RunError> {
    let prepared = PreparedBrowserRun::read(root, image)?;
    match execution {
        Execution::DryRun => {
            prepared.print();
            Ok(())
        }
        Execution::Run => {
            run_prepared_with(prepared, |command, budget, _| perform(command, budget)).map(|_| ())
        }
    }
}

pub fn execute(execution: Execution, image: &str) -> Result<(), RunError> {
    match execution {
        Execution::DryRun => execute_with(&std::env::current_dir()?, execution, image, run),
        Execution::Run => {
            let prepared = PreparedBrowserRun::read(&std::env::current_dir()?, image)?;
            prepared.run().map(|_| ())
        }
    }
}

#[cfg(unix)]
pub fn execute_comparison(
    execution: Execution,
    image: &str,
    reference: &str,
) -> Result<(), RunError> {
    let prepared =
        PreparedBrowserRun::read(&std::env::current_dir()?, image)?.with_reference(reference)?;
    execute_prepared(execution, prepared)
}

#[cfg(unix)]
pub fn execute_packaged_comparison(
    execution: Execution,
    image: &str,
    reference: &str,
    target: ServerTarget,
    package: &Path,
    order: PackagedOrder,
) -> Result<(), RunError> {
    if !matches!(target, ServerTarget::LinuxArm64 | ServerTarget::LinuxX86_64) {
        return Err(io::Error::other(
            "packaged browser comparison requires a Linux server candidate",
        )
        .into());
    }
    let candidate = ServerCandidate::read(package, target).map_err(|error| {
        io::Error::other(format!(
            "cannot read packaged browser server candidate ({error})"
        ))
    })?;
    let prepared = PreparedBrowserRun::read(&std::env::current_dir()?, image)?
        .with_comparison(reference, Some(PackagedComparison { candidate, order }))?;
    execute_prepared(execution, prepared)
}

#[cfg(unix)]
fn execute_prepared(execution: Execution, prepared: PreparedBrowserRun) -> Result<(), RunError> {
    match execution {
        Execution::DryRun => {
            prepared.print();
            Ok(())
        }
        Execution::Run => prepared.run().map(|_| ()),
    }
}
