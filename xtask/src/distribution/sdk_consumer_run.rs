use super::{
    check_sdk_consumer_lock, check_sdk_consumer_metadata, SdkConsumerAction, SdkConsumerLockError,
    SdkConsumerPhase, SdkConsumerScope, SdkConsumerWorkspace, SdkContents, SdkContentsError,
    SdkHttpTarget, SdkMetadataError, SdkPackage,
};
use crate::process::{CommandSupervisor, ProcessError};
use std::{
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub enum SdkConsumerRunError {
    Io(std::io::Error),
    Contents(SdkContentsError),
    Process {
        phase: SdkConsumerPhase,
        source: ProcessError,
    },
    Lock(SdkConsumerLockError),
    Metadata(SdkMetadataError),
    Output,
    Finalization {
        context: &'static str,
        source: std::io::Error,
    },
    FollowUp {
        operation: String,
        context: &'static str,
        source: std::io::Error,
    },
}
impl std::fmt::Display for SdkConsumerRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Contents(error) => error.fmt(formatter),
            Self::Process { phase, source } => {
                write!(formatter, "SDK consumer phase {}: {source}", phase.label())
            }
            Self::Lock(error) => error.fmt(formatter),
            Self::Metadata(error) => error.fmt(formatter),
            Self::Output => {
                formatter.write_str("SDK consumer requires its exact successful contract output")
            }
            Self::Finalization { context, source } => write!(formatter, "{context}: {source}"),
            Self::FollowUp {
                operation,
                context,
                source,
            } => write!(formatter, "{operation}; {context}: {source}"),
        }
    }
}
impl std::error::Error for SdkConsumerRunError {}
impl From<std::io::Error> for SdkConsumerRunError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) trait SdkPort {
    fn run(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError>;
    fn finish(self) -> Result<(), ProcessError>;
}

impl SdkPort for CommandSupervisor {
    fn run(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError> {
        CommandSupervisor::run(self, command, budget)
    }

    fn finish(self) -> Result<(), ProcessError> {
        CommandSupervisor::finish(self)
    }
}

struct Callback<F> {
    perform: F,
}

impl<F> SdkPort for Callback<F>
where
    F: FnMut(&mut Command, Duration) -> Result<(), ProcessError>,
{
    fn run(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError> {
        (self.perform)(command, budget)
    }

    fn finish(self) -> Result<(), ProcessError> {
        Ok(())
    }
}

fn invalid(message: &str) -> std::io::Error {
    std::io::Error::other(message)
}

fn ensure_directory(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(invalid(
            "consumer artifact parents must remain directories without symlinks",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(path),
        Err(error) => Err(error),
    }
}

pub fn verify_sdk_with(
    root: &Path,
    candidate: &Path,
    perform: impl FnMut(&mut Command, Duration) -> Result<(), ProcessError>,
) -> Result<std::path::PathBuf, SdkConsumerRunError> {
    let contents = SdkContents::read(candidate).map_err(SdkConsumerRunError::Contents)?;
    let mut parent = std::fs::canonicalize(root)?;
    for component in [".cache", "distributions", "sdk-consumers"] {
        parent.push(component);
        ensure_directory(&parent)?;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let identity = format!("{}-{stamp}", std::process::id());
    let artifacts = parent.join(&identity);
    let execution = std::fs::canonicalize(std::env::temp_dir())?
        .join(format!("rusterisk-sdk-consumers-{identity}"));
    println!("SDK consumer artifacts: {}", artifacts.display());
    println!("SDK consumer source: {}", contents.source_digest());
    std::io::stdout().flush()?;
    verify_sdk_consumers_with(&contents, &execution, &artifacts, perform)?;
    println!("SDK candidate consumers passed");
    Ok(artifacts)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| invalid("consumer artifact parent missing"))?,
    )?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)
}
pub(super) fn read(path: &Path, limit: u64) -> Result<Vec<u8>, std::io::Error> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > limit {
        return Err(invalid(
            "consumer artifacts must remain bounded regular files",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("consumer artifact size exceeded"));
    }
    Ok(bytes)
}
pub(super) fn stdout(phase: SdkConsumerPhase) -> String {
    let suffix = match phase.action() {
        SdkConsumerAction::Resolve | SdkConsumerAction::Metadata => "json",
        SdkConsumerAction::Build | SdkConsumerAction::Run => "txt",
    };
    format!("{}.stdout.{suffix}", phase.label())
}
pub(super) fn stderr(phase: SdkConsumerPhase) -> String {
    format!("{}.stderr.txt", phase.label())
}

pub(super) fn follow_up<T>(
    operation: Result<T, SdkConsumerRunError>,
    follow_up: Result<(), std::io::Error>,
    context: &'static str,
) -> Result<T, SdkConsumerRunError> {
    match (operation, follow_up) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(source)) => Err(SdkConsumerRunError::Finalization { context, source }),
        (Err(operation), Err(source)) => Err(SdkConsumerRunError::FollowUp {
            operation: operation.to_string(),
            context,
            source,
        }),
    }
}

pub(super) fn phase(
    workspace: &SdkConsumerWorkspace<'_>,
    phase: SdkConsumerPhase,
    perform: &mut impl FnMut(&mut Command, Duration) -> Result<(), ProcessError>,
) -> Result<(), SdkConsumerRunError> {
    phase_with_target(workspace, phase, None, perform)
}

pub(super) fn phase_with_target(
    workspace: &SdkConsumerWorkspace<'_>,
    phase: SdkConsumerPhase,
    target: Option<&SdkHttpTarget<'_>>,
    perform: &mut impl FnMut(&mut Command, Duration) -> Result<(), ProcessError>,
) -> Result<(), SdkConsumerRunError> {
    workspace.check_sources()?;
    let plan = workspace.plan();
    let http_run = workspace.runtime().is_some()
        && phase.scope() == SdkConsumerScope::Sdk
        && phase.action() == SdkConsumerAction::Run;
    if target.is_some() != http_run {
        return Err(invalid("HTTP consumer credentials belong only to its runtime phase").into());
    }
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(plan.state().join(stdout(phase)))?;
    let diagnostic = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(plan.state().join(stderr(phase)))?;
    let step = plan.step(phase);
    let mut command = Command::new(plan.program(phase));
    command
        .args(step.arguments)
        .current_dir(plan.directory(phase.scope()))
        .env("CARGO_TARGET_DIR", plan.target(phase.scope()))
        .env("CARGO_BUILD_JOBS", "2")
        .env("CARGO_NET_OFFLINE", "true")
        .stdin(Stdio::null())
        .stdout(output)
        .stderr(diagnostic);
    let budget = if let Some(target) = target {
        target.configure(&mut command);
        Duration::from_secs(60)
    } else {
        step.budget
    };
    let started = Instant::now();
    let result = perform(&mut command, budget)
        .map_err(|source| SdkConsumerRunError::Process { phase, source });
    println!(
        "SDK consumer phase={} elapsed={:.3}s",
        phase.label(),
        started.elapsed().as_secs_f64()
    );
    follow_up(
        result,
        workspace.check_sources(),
        "consumer source verification failed",
    )
}

pub(super) fn seed(
    contents: &SdkContents,
    scope: SdkConsumerScope,
) -> Result<&[u8], std::io::Error> {
    let package = match scope {
        SdkConsumerScope::CoreMsrv => SdkPackage::Core,
        SdkConsumerScope::Sdk => SdkPackage::Client,
    };
    contents
        .archive(package)
        .files()
        .iter()
        .find(|file| file.path().as_str() == "Cargo.lock")
        .map(|file| file.bytes())
        .ok_or_else(|| invalid("candidate lockfile missing"))
}

pub(super) fn lock_unchanged(
    workspace: &SdkConsumerWorkspace<'_>,
    scope: SdkConsumerScope,
    expected: &[u8],
) -> Result<(), std::io::Error> {
    if read(
        &workspace.plan().directory(scope).join("Cargo.lock"),
        2 * 1024 * 1024,
    )? != expected
    {
        return Err(invalid(
            "locked consumer command changed its dependency identities",
        ));
    }
    Ok(())
}

pub(super) fn check_run_output(
    directory: &Path,
    scope: SdkConsumerScope,
) -> Result<(), SdkConsumerRunError> {
    let phase = SdkConsumerPhase::new(scope, SdkConsumerAction::Run);
    let output = read(&directory.join(stdout(phase)), 4096)?;
    let diagnostic = read(&directory.join(stderr(phase)), 4096)?;
    let expected = match scope {
        SdkConsumerScope::CoreMsrv => "core candidate consumer passed\n",
        SdkConsumerScope::Sdk => "SDK candidate consumer passed\n",
    };
    if output != expected.as_bytes() || !diagnostic.is_empty() {
        return Err(SdkConsumerRunError::Output);
    }
    Ok(())
}

fn run(
    contents: &SdkContents,
    workspace: &SdkConsumerWorkspace<'_>,
    mut perform: impl FnMut(&mut Command, Duration) -> Result<(), ProcessError>,
) -> Result<(), SdkConsumerRunError> {
    for scope in SdkConsumerScope::ALL {
        phase(
            workspace,
            SdkConsumerPhase::new(scope, SdkConsumerAction::Resolve),
            &mut perform,
        )?;
        let lock = read(
            &workspace.plan().directory(scope).join("Cargo.lock"),
            2 * 1024 * 1024,
        )?;
        check_sdk_consumer_lock(scope, contents.version(), seed(contents, scope)?, &lock)
            .map_err(SdkConsumerRunError::Lock)?;
        let metadata = SdkConsumerPhase::new(scope, SdkConsumerAction::Metadata);
        phase(workspace, metadata, &mut perform)?;
        lock_unchanged(workspace, scope, &lock)?;
        let bytes = read(
            &workspace.plan().state().join(stdout(metadata)),
            8 * 1024 * 1024,
        )?;
        check_sdk_consumer_metadata(workspace.plan(), scope, contents.version(), &bytes)
            .map_err(SdkConsumerRunError::Metadata)?;
        phase(
            workspace,
            SdkConsumerPhase::new(scope, SdkConsumerAction::Build),
            &mut perform,
        )?;
        lock_unchanged(workspace, scope, &lock)?;
        let execute = SdkConsumerPhase::new(scope, SdkConsumerAction::Run);
        phase(workspace, execute, &mut perform)?;
        lock_unchanged(workspace, scope, &lock)?;
        check_run_output(workspace.plan().state(), scope)?;
    }
    Ok(())
}

pub(super) fn preserve(
    workspace: &SdkConsumerWorkspace<'_>,
    artifacts: &Path,
) -> Result<(), std::io::Error> {
    for phase in SdkConsumerPhase::ALL {
        for file in [stdout(phase), stderr(phase)] {
            let path = workspace.plan().state().join(&file);
            match std::fs::symlink_metadata(&path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            }
            write_new(&artifacts.join(file), &read(&path, 8 * 1024 * 1024)?)?;
        }
    }
    for scope in SdkConsumerScope::ALL {
        write_new(
            &artifacts
                .join("consumers")
                .join(scope.label())
                .join("Cargo.lock"),
            &read(
                &workspace.plan().directory(scope).join("Cargo.lock"),
                2 * 1024 * 1024,
            )?,
        )?;
    }
    Ok(())
}

pub fn verify_sdk_consumers_with(
    contents: &SdkContents,
    execution: &Path,
    artifacts: &Path,
    perform: impl FnMut(&mut Command, Duration) -> Result<(), ProcessError>,
) -> Result<(), SdkConsumerRunError> {
    verify_sdk_consumers_supervised(contents, execution, artifacts, Callback { perform })
}

pub(crate) fn verify_sdk_consumers_supervised(
    contents: &SdkContents,
    execution: &Path,
    artifacts: &Path,
    mut port: impl SdkPort,
) -> Result<(), SdkConsumerRunError> {
    let workspace = match create_artifacts(contents, execution, artifacts).and_then(|()| {
        SdkConsumerWorkspace::create(contents, execution).map_err(SdkConsumerRunError::Io)
    }) {
        Ok(workspace) => workspace,
        Err(error) => {
            return follow_up(
                Err(error),
                port.finish().map_err(std::io::Error::other),
                "consumer supervisor cleanup failed",
            )
        }
    };
    let operation = run(contents, &workspace, |command, budget| {
        port.run(command, budget)
    });
    let operation = follow_up(
        operation,
        port.finish().map_err(std::io::Error::other),
        "consumer supervisor cleanup failed",
    );
    let operation = follow_up(
        operation,
        workspace.check_sources(),
        "consumer source verification failed",
    );
    let operation = follow_up(
        operation,
        preserve(&workspace, artifacts),
        "consumer artifact preservation failed",
    );
    follow_up(
        operation,
        workspace.close(),
        "consumer working copy cleanup failed",
    )
}

pub(super) fn create_artifacts(
    contents: &SdkContents,
    execution: &Path,
    artifacts: &Path,
) -> Result<(), SdkConsumerRunError> {
    check_roots(execution, artifacts)?;
    std::fs::create_dir(artifacts)?;
    write_new(
        &artifacts.join("SOURCE.sha256"),
        format!("{}\n", contents.source_digest()).as_bytes(),
    )?;
    let sums: String = SdkPackage::ALL
        .iter()
        .map(|package| {
            format!(
                "{}  {}-{}.crate\n",
                contents.checksum(*package),
                package.name(),
                contents.version()
            )
        })
        .collect();
    write_new(&artifacts.join("SHA256SUMS"), sums.as_bytes())?;
    Ok(())
}

fn check_roots(execution: &Path, artifacts: &Path) -> Result<(), std::io::Error> {
    if !execution.is_absolute()
        || !artifacts.is_absolute()
        || execution.starts_with(artifacts)
        || artifacts.starts_with(execution)
        || [execution, artifacts].iter().any(|path| {
            path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
        })
    {
        return Err(invalid(
            "consumer execution and artifact roots must be separate absolute paths",
        ));
    }
    let parent = artifacts
        .parent()
        .ok_or_else(|| invalid("consumer artifact parent missing"))?;
    if std::fs::canonicalize(parent)? != parent {
        return Err(invalid(
            "consumer artifacts require a physical parent directory",
        ));
    }
    Ok(())
}
