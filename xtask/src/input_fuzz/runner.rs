use super::{
    snapshot::{read_directory, read_under, write_new},
    validate_inventory, validate_run, CapturedInputFuzz, Execution, FuzzError, FuzzPlan,
    LinuxTarget, Phase, Target, MAX_INPUT,
};
use crate::{
    process::{CommandSupervisor, ProcessError},
    reference::ImageIdentity,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandMode {
    Work,
    Cleanup,
}
pub struct TargetCompletion {
    target: Target,
    executions: u64,
}
impl TargetCompletion {
    pub fn target(&self) -> Target {
        self.target
    }
    pub fn executions(&self) -> u64 {
        self.executions
    }
}
struct FuzzIdentity {
    capture: CapturedInputFuzz,
    image: ImageIdentity,
    architecture: LinuxTarget,
}

/// Single-target validation cannot complete all-target validation.
///
/// ```compile_fail
/// use xtask::input_fuzz::{InputFuzzOutcome, InputFuzzTargetOutcome};
/// fn require_all(_: InputFuzzOutcome) {}
/// fn partial(outcome: InputFuzzTargetOutcome) { require_all(outcome); }
/// ```
pub struct InputFuzzOutcome {
    identity: FuzzIdentity,
    runs: [TargetCompletion; Target::ALL.len()],
}
impl InputFuzzOutcome {
    pub fn inputs(&self) -> impl ExactSizeIterator<Item = (&Path, &[u8])> {
        self.identity.capture.inputs()
    }
    pub fn image(&self) -> &str {
        self.identity.image.as_str()
    }
    pub fn architecture(&self) -> LinuxTarget {
        self.identity.architecture
    }
    pub fn scope(&self) -> Scope {
        Scope::All
    }
    pub fn runs(&self) -> &[TargetCompletion; Target::ALL.len()] {
        &self.runs
    }
}
pub struct InputFuzzTargetOutcome {
    identity: FuzzIdentity,
    run: TargetCompletion,
}
impl InputFuzzTargetOutcome {
    pub fn inputs(&self) -> impl ExactSizeIterator<Item = (&Path, &[u8])> {
        self.identity.capture.inputs()
    }
    pub fn image(&self) -> &str {
        self.identity.image.as_str()
    }
    pub fn architecture(&self) -> LinuxTarget {
        self.identity.architecture
    }
    pub fn scope(&self) -> Scope {
        Scope::Target(self.run.target)
    }
    pub fn run(&self) -> &TargetCompletion {
        &self.run
    }
}
struct CheckedFuzz {
    identity: FuzzIdentity,
    scope: Scope,
    runs: Vec<TargetCompletion>,
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
fn follow<T>(
    operation: Result<T, FuzzError>,
    secondary: Result<(), FuzzError>,
    context: &'static str,
) -> Result<T, FuzzError> {
    match (operation, secondary) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(follow_up)) => Err(FuzzError::FollowUp {
            operation: operation.to_string(),
            context,
            follow_up: follow_up.to_string(),
        }),
    }
}
fn finish_all(
    operation: Result<CheckedFuzz, FuzzError>,
    cleanup: Result<(), ProcessError>,
) -> Result<InputFuzzOutcome, FuzzError> {
    let checked = follow(
        operation,
        cleanup.map_err(FuzzError::Process),
        "input fuzz supervisor cleanup failed",
    )?;
    if checked.scope != Scope::All
        || checked.runs.len() != Target::ALL.len()
        || checked
            .runs
            .iter()
            .zip(Target::ALL)
            .any(|(run, target)| run.target != target)
    {
        return Err(FuzzError::Results);
    }
    Ok(InputFuzzOutcome {
        identity: checked.identity,
        runs: checked.runs.try_into().map_err(|_| FuzzError::Results)?,
    })
}
fn finish_target(
    operation: Result<CheckedFuzz, FuzzError>,
    cleanup: Result<(), ProcessError>,
) -> Result<InputFuzzTargetOutcome, FuzzError> {
    let checked = follow(
        operation,
        cleanup.map_err(FuzzError::Process),
        "input fuzz supervisor cleanup failed",
    )?;
    let Scope::Target(target) = checked.scope else {
        return Err(FuzzError::Results);
    };
    let [run]: [TargetCompletion; 1] = checked.runs.try_into().map_err(|_| FuzzError::Results)?;
    if run.target != target {
        return Err(FuzzError::Results);
    }
    Ok(InputFuzzTargetOutcome {
        identity: checked.identity,
        run,
    })
}

impl CapturedInputFuzz {
    pub fn verify_all(
        self,
        image: &str,
        architecture: LinuxTarget,
    ) -> Result<InputFuzzOutcome, FuzzError> {
        let mut supervisor = CommandSupervisor::new().map_err(FuzzError::Process)?;
        let operation = verify_with(
            self,
            Scope::All,
            image,
            architecture,
            |command, budget, mode| match mode {
                CommandMode::Work => supervisor.run(command, budget),
                CommandMode::Cleanup => supervisor.run_cleanup(command, budget),
            },
        );
        finish_all(operation, supervisor.finish())
    }
    pub fn verify_target(
        self,
        target: Target,
        image: &str,
        architecture: LinuxTarget,
    ) -> Result<InputFuzzTargetOutcome, FuzzError> {
        let mut supervisor = CommandSupervisor::new().map_err(FuzzError::Process)?;
        let operation = verify_with(
            self,
            Scope::Target(target),
            image,
            architecture,
            |command, budget, mode| match mode {
                CommandMode::Work => supervisor.run(command, budget),
                CommandMode::Cleanup => supervisor.run_cleanup(command, budget),
            },
        );
        finish_target(operation, supervisor.finish())
    }
}
fn reserve(parent: &Path, identity: &str) -> Result<(String, PathBuf), FuzzError> {
    for attempt in 0..32 {
        let identity = if attempt == 0 {
            identity.to_owned()
        } else {
            format!("{identity}-{attempt}")
        };
        let path = parent.join(format!("rusterisk-input-fuzz-{identity}"));
        match fs::create_dir(&path) {
            Ok(()) => return Ok((identity, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "input fuzz directory reservation limit exceeded",
    )
    .into())
}
fn ensure_directory(path: &Path) -> Result<(), FuzzError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(path)?.is_dir() {
                Ok(())
            } else {
                Err(FuzzError::Results)
            }
        }
        Err(error) => Err(error.into()),
    }
}
fn artifact_path(target: Target) -> PathBuf {
    Path::new("state/artifacts").join(target.name())
}
fn preserve(
    root: &Path,
    directory: &Path,
    identity: &str,
    target: Target,
) -> Result<(), FuzzError> {
    if !fs::symlink_metadata(root)?.is_dir() {
        return Err(FuzzError::Results);
    }
    let relative = artifact_path(target);
    for (index, entry) in read_directory(directory, &relative)?.enumerate() {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or(FuzzError::Results)?;
        if index >= 16
            || name.len() > 160
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(FuzzError::Results);
        }
        let bytes = read_under(directory, &relative.join(name), MAX_INPUT)?;
        let mut destination = root.to_owned();
        for component in [".cache", "input-fuzz-crashes", identity, target.name()] {
            destination.push(component);
            ensure_directory(&destination)?;
        }
        let path = destination.join(name);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        std::io::Write::write_all(&mut file, &bytes)?;
        eprintln!("fuzz failing input preserved at {}", path.display());
    }
    Ok(())
}
fn phases(scope: Scope) -> Result<Vec<Phase>, FuzzError> {
    let mut phases: Vec<_> = [
        Phase::RustVersion,
        Phase::Version,
        Phase::Inventory,
        Phase::Fetch,
    ]
    .into();
    for target in scope.targets() {
        let seeds = target.seeds();
        if seeds.is_empty()
            || !seeds.iter().any(|seed| seed.valid)
            || !seeds.iter().any(|seed| !seed.valid)
            || seeds
                .iter()
                .any(|seed| seed.bytes.is_empty() || seed.bytes.len() > MAX_INPUT)
        {
            return Err(FuzzError::Inventory);
        }
        phases.extend([Phase::Build(target), Phase::Run(target)]);
    }
    Ok(phases)
}
fn plan(
    identity: &str,
    directory: &Path,
    image: &str,
    architecture: LinuxTarget,
) -> Result<FuzzPlan, FuzzError> {
    FuzzPlan::new(
        identity,
        directory.join("source").to_str().ok_or(FuzzError::Plan)?,
        directory.join("state").to_str().ok_or(FuzzError::Plan)?,
        image,
        architecture,
    )
}
fn print(capture: &CapturedInputFuzz, scope: Scope, plan: &FuzzPlan, phases: &[Phase]) {
    println!("input fuzz: {} targets; snapshot <= {} MiB; project files <= {}; vendor files <= {}; native output <= 4 MiB; explicit production path dependencies",
        scope.targets().len(), super::snapshot::MAX_SNAPSHOT_BYTES / (1024 * 1024),
        super::snapshot::MAX_PROJECT_FILES, super::snapshot::MAX_VENDOR_FILES);
    println!("{}", capture.normalized());
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
}
fn run_phase(
    plan: &FuzzPlan,
    phase: Phase,
    state: &Path,
    perform: &mut impl FnMut(&mut Command, Duration, CommandMode) -> Result<(), ProcessError>,
) -> Result<(), FuzzError> {
    let step = plan.step(phase);
    let capture = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(state.join(format!("{}.txt", phase.label())))?;
    let mut command = Command::new("docker");
    command
        .args(step.arguments)
        .stdout(capture.try_clone()?)
        .stderr(capture);
    let started = std::time::Instant::now();
    let result = perform(&mut command, step.budget, CommandMode::Work);
    println!(
        "phase={} elapsed={:.3}s",
        phase.label(),
        started.elapsed().as_secs_f64()
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
    combine(result.map_err(FuzzError::Process), cleanup)
}
fn read_output(
    operation: Result<(), FuzzError>,
    directory: &Path,
    phase: Phase,
    diagnostics: &mut impl io::Write,
) -> Result<Vec<u8>, FuzzError> {
    let bytes = read_under(
        directory,
        &Path::new("state").join(format!("{}.txt", phase.label())),
        4 * 1024 * 1024,
    );
    match (operation, bytes) {
        (Ok(()), Ok(bytes)) => Ok(bytes),
        (Err(error), Ok(bytes)) => fail_with_diagnostic(error, phase, Some(&bytes), diagnostics),
        (Ok(()), Err(error)) => fail_with_diagnostic(error, phase, None, diagnostics),
        (Err(operation), Err(follow_up)) => fail_with_diagnostic(
            FuzzError::FollowUp {
                operation: operation.to_string(),
                context: "input fuzz native output read failed",
                follow_up: follow_up.to_string(),
            },
            phase,
            None,
            diagnostics,
        ),
    }
}

fn write_diagnostic(
    phase: Phase,
    bytes: Option<&[u8]>,
    diagnostics: &mut impl io::Write,
) -> Result<(), FuzzError> {
    const MAXIMUM: usize = 64 * 1024;
    writeln!(
        diagnostics,
        "input fuzz phase={}: native output (up to {MAXIMUM} bytes)",
        phase.label()
    )?;
    let bytes = bytes.unwrap_or(b"native output unavailable");
    let prefix = &bytes[..bytes.len().min(MAXIMUM)];
    let text = String::from_utf8_lossy(prefix);
    let mut length = text.len().min(MAXIMUM);
    while !text.is_char_boundary(length) {
        length -= 1;
    }
    diagnostics.write_all(&text.as_bytes()[..length])?;
    writeln!(diagnostics)?;
    if bytes.len() > prefix.len() || text.len() > length {
        writeln!(diagnostics, "[native output truncated]")?;
    }
    Ok(())
}

fn fail_with_diagnostic<T>(
    error: FuzzError,
    phase: Phase,
    bytes: Option<&[u8]>,
    diagnostics: &mut impl io::Write,
) -> Result<T, FuzzError> {
    follow(
        Err(error),
        write_diagnostic(phase, bytes, diagnostics),
        "input fuzz native diagnostic failed",
    )
}

fn verify_with(
    capture: CapturedInputFuzz,
    scope: Scope,
    image: &str,
    architecture: LinuxTarget,
    perform: impl FnMut(&mut Command, Duration, CommandMode) -> Result<(), ProcessError>,
) -> Result<CheckedFuzz, FuzzError> {
    verify_with_diagnostics(
        capture,
        scope,
        image,
        architecture,
        perform,
        &mut io::stderr(),
    )
}

fn verify_with_diagnostics(
    capture: CapturedInputFuzz,
    scope: Scope,
    image: &str,
    architecture: LinuxTarget,
    mut perform: impl FnMut(&mut Command, Duration, CommandMode) -> Result<(), ProcessError>,
    diagnostics: &mut impl io::Write,
) -> Result<CheckedFuzz, FuzzError> {
    let image = ImageIdentity::parse(image).ok_or(FuzzError::Plan)?;
    let phases = phases(scope)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| FuzzError::Plan)?
        .as_nanos();
    let (identity, directory) = reserve(
        &std::env::temp_dir(),
        &format!("fuzz-{}-{stamp}", std::process::id()),
    )?;
    let source = directory.join("source");
    let state = directory.join("state");
    let operation = (|| {
        for name in ["cargo", "tmp", "target", "artifacts", "corpus"] {
            fs::create_dir_all(state.join(name))?;
        }
        capture.write(&source, &state)?;
        for target in scope.targets() {
            fs::create_dir_all(state.join("artifacts").join(target.name()))?;
            for seed in target.seeds() {
                write_new(
                    &state.join("corpus").join(target.name()).join(seed.name),
                    seed.bytes,
                )?;
            }
        }
        let plan = plan(&identity, &directory, image.as_str(), architecture)?;
        print(&capture, scope, &plan, &phases);
        let mut runs = Vec::new();
        for phase in phases {
            let checked = follow(
                run_phase(&plan, phase, &state, &mut perform),
                capture.check_source(&source),
                "input fuzz source check failed",
            );
            let checked = follow(
                checked,
                capture.check_harness(&state),
                "input fuzz harness check failed",
            );
            let operation = (|| {
                let bytes = read_output(checked, &directory, phase, diagnostics)?;
                let validation = (|| {
                    match phase {
                        Phase::RustVersion => {
                            let text =
                                std::str::from_utf8(&bytes).map_err(|_| FuzzError::Results)?;
                            if !text.lines().any(|line| {
                                line == "commit-hash: 8925ea358a0f265ca61026aadc7ecc506c545cbe"
                            }) || !text
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
                        Phase::Fetch | Phase::Build(_) => {}
                        Phase::Run(target) => {
                            let executions = validate_run(&bytes, target.seeds().len())?;
                            if read_directory(&directory, &artifact_path(target))?
                                .next()
                                .transpose()?
                                .is_some()
                            {
                                return Err(FuzzError::Results);
                            }
                            runs.push(TargetCompletion { target, executions });
                            println!(
                                "input fuzz: {} completed {executions} executions",
                                target.name()
                            );
                        }
                    }
                    Ok(())
                })();
                match validation {
                    Ok(()) => Ok(()),
                    Err(error) => fail_with_diagnostic(error, phase, Some(&bytes), diagnostics),
                }
            })();
            if let Err(error) = operation {
                return if let Phase::Run(target) = phase {
                    follow(
                        Err(error),
                        preserve(capture.root(), &directory, &identity, target),
                        "input fuzz failing input preservation failed",
                    )
                } else {
                    Err(error)
                };
            }
        }
        follow(
            capture.check_source(&source),
            capture.check_harness(&state),
            "final input fuzz harness check failed",
        )?;
        Ok(runs)
    })();
    follow(
        operation,
        fs::remove_dir_all(directory).map_err(FuzzError::Io),
        "input fuzz temporary cleanup failed",
    )
    .map(|runs| CheckedFuzz {
        identity: FuzzIdentity {
            capture,
            image,
            architecture,
        },
        scope,
        runs,
    })
}

/// Planning doesn't produce a completed fuzz outcome.
///
/// ```compile_fail
/// use xtask::input_fuzz::{execute, Scope, Execution, FuzzError, InputFuzzOutcome};
/// fn synthetic(image: &str) -> Result<InputFuzzOutcome, FuzzError> {
///     execute(Scope::All, Execution::DryRun, image)
/// }
/// ```
pub fn execute(scope: Scope, execution: Execution, image: &str) -> Result<(), FuzzError> {
    let capture = CapturedInputFuzz::read(&std::env::current_dir()?)?;
    let architecture = LinuxTarget::host()?;
    match execution {
        Execution::DryRun => {
            let directory = std::env::temp_dir().join("rusterisk-input-fuzz-preview");
            let plan = plan("preview", &directory, image, architecture)?;
            print(&capture, scope, &plan, &phases(scope)?);
            Ok(())
        }
        Execution::Run => {
            match scope {
                Scope::All => {
                    capture.verify_all(image, architecture)?;
                }
                Scope::Target(target) => {
                    capture.verify_target(target, image, architecture)?;
                }
            }
            println!("input fuzz: every selected target completed without crashes");
            Ok(())
        }
    }
}
#[cfg(test)]
mod tests;
