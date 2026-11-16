use super::{
    native_archives, validate_instrumented_archive, validate_rejection, Defect, NativeArchive,
    NativeInventory, NativeSanitizerPlan, Phase, Scope,
};
use crate::{
    distribution::{SnapshotError, SourceSnapshot},
    process::ProcessError,
};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandMode {
    Work,
    Cleanup,
}

#[derive(Debug)]
pub enum NativeError {
    Io(io::Error),
    Source(SnapshotError),
    Process {
        phase: Phase,
        error: ProcessError,
    },
    Check {
        phase: Phase,
        error: io::Error,
    },
    FollowUp {
        operation: String,
        context: &'static str,
        error: io::Error,
    },
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Source(error) => error.fmt(formatter),
            Self::Process { phase, error } => {
                write!(formatter, "native {}: {error}", phase.label())
            }
            Self::Check { phase, error } => write!(formatter, "native {}: {error}", phase.label()),
            Self::FollowUp {
                operation,
                context,
                error,
            } => write!(formatter, "{operation}; {context}: {error}"),
        }
    }
}

impl std::error::Error for NativeError {}
impl From<io::Error> for NativeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<SnapshotError> for NativeError {
    fn from(error: SnapshotError) -> Self {
        Self::Source(error)
    }
}

fn follow<T>(
    operation: Result<T, NativeError>,
    cleanup: io::Result<()>,
    context: &'static str,
) -> Result<T, NativeError> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(error.into()),
        (Err(operation), Err(error)) => Err(NativeError::FollowUp {
            operation: operation.to_string(),
            context,
            error,
        }),
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)
}

fn output(path: &Path) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > 4 * 1024 * 1024 {
        return Err(io::Error::other(
            "native output exceeds four mebibytes or is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(io::Error::other("native output exceeds four mebibytes"));
    }
    Ok(bytes)
}

fn compiler(phase: Phase, bytes: &[u8]) -> io::Result<()> {
    let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let valid = match phase {
        Phase::ClangVersion => text
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("Debian clang version 14.0.6")),
        Phase::RustVersion => {
            let host = match std::env::consts::ARCH {
                "aarch64" => "host: aarch64-unknown-linux-gnu",
                "x86_64" => "host: x86_64-unknown-linux-gnu",
                _ => return Err(io::Error::other("unsupported native sanitizer host")),
            };
            text.lines().any(|line| line == "release: 1.99.0")
                && text.lines().any(|line| line == host)
        }
        Phase::CompileProbe(_)
        | Phase::RejectProbe(_)
        | Phase::InspectArchive(_)
        | Phase::Fetch
        | Phase::Build(_)
        | Phase::Inventory(_)
        | Phase::Run(_) => true,
    };
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(
            "native compiler identity does not match the pinned image",
        ))
    }
}

fn phase(
    plan: &NativeSanitizerPlan,
    selected: Phase,
    source: &Path,
    destination: &Path,
    perform: &mut impl FnMut(&mut Command, Duration, CommandMode) -> Result<(), ProcessError>,
) -> Result<Vec<u8>, NativeError> {
    let step = plan.step(selected);
    let path = destination.join(format!("{}.txt", selected.label()));
    let capture = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let mut command = Command::new("docker");
    command
        .args(&step.arguments)
        .current_dir(source)
        .env("MISE_TRUSTED_CONFIG_PATHS", source)
        .stdout(capture.try_clone()?)
        .stderr(capture);
    let started = std::time::Instant::now();
    let result = perform(&mut command, step.budget, CommandMode::Work);
    println!(
        "native phase={} elapsed={:.3}s",
        selected.label(),
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
            let cleanup = plan.cleanup(selected);
            perform(
                Command::new("docker").args(&cleanup.arguments),
                cleanup.budget,
                CommandMode::Cleanup,
            )
            .map_err(io::Error::other)
        }
    };
    let bytes = output(&path);
    let operation = match (selected, result, bytes) {
        (Phase::RejectProbe(defect), result, Ok(bytes)) => {
            let code = match result {
                Ok(()) => Some(0),
                Err(ProcessError::Status(status)) => status.code(),
                Err(error) => {
                    return follow(
                        Err(NativeError::Process {
                            phase: selected,
                            error,
                        }),
                        cleanup,
                        "native container cleanup failed",
                    )
                }
            };
            validate_rejection(defect, code, &bytes)
                .map(|()| bytes)
                .map_err(|error| NativeError::Check {
                    phase: selected,
                    error,
                })
        }
        (_, Ok(()), Ok(bytes)) => {
            compiler(selected, &bytes)
                .map(|()| bytes)
                .map_err(|error| NativeError::Check {
                    phase: selected,
                    error,
                })
        }
        (_, Err(error), Ok(bytes)) => {
            eprintln!("{}", String::from_utf8_lossy(&bytes));
            Err(NativeError::Process {
                phase: selected,
                error,
            })
        }
        (_, result, Err(error)) => match result {
            Ok(()) => Err(NativeError::Check {
                phase: selected,
                error,
            }),
            Err(process) => Err(NativeError::FollowUp {
                operation: NativeError::Process {
                    phase: selected,
                    error: process,
                }
                .to_string(),
                context: "native output preservation failed",
                error,
            }),
        },
    };
    follow(operation, cleanup, "native container cleanup failed")
}

fn copy_archive(archive: &NativeArchive, state: &Path) -> io::Result<()> {
    let mut selected = None;
    for candidate in archive.paths() {
        let relative = candidate
            .strip_prefix("/state/")
            .ok_or_else(|| io::Error::other("native archive escaped state"))?;
        let mut path = state.to_owned();
        let components: Vec<_> = Path::new(relative).components().collect();
        for component in components.iter().take(components.len().saturating_sub(1)) {
            path.push(component);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_dir() => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                Ok(_) => {
                    return Err(io::Error::other(
                        "native archive parent is not a regular directory",
                    ))
                }
                Err(error) => return Err(error),
            }
        }
        let path = state.join(relative);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
            Ok(metadata)
                if metadata.file_type().is_file()
                    && metadata.len() > 0
                    && metadata.len() <= 512 * 1024 * 1024 =>
            {
                if selected.replace((path, metadata.len())).is_some() {
                    return Err(io::Error::other(
                        "native build linked ambiguous archive copies",
                    ));
                }
            }
            Ok(_) => {
                return Err(io::Error::other(
                    "native archive is not a bounded regular file",
                ))
            }
        }
    }
    let (path, size) =
        selected.ok_or_else(|| io::Error::other("linked native archive is missing"))?;
    let mut target = fs::OpenOptions::new().write(true).create_new(true).open(
        state
            .join("native")
            .join(format!("lib-{}.a", archive.library().label())),
    )?;
    let copied = io::copy(
        &mut fs::File::open(path)?.take(512 * 1024 * 1024 + 1),
        &mut target,
    )?;
    if copied != size || copied > 512 * 1024 * 1024 {
        return Err(io::Error::other(
            "native archive changed during inspection capture",
        ));
    }
    Ok(())
}

pub struct CapturedNative {
    source: SourceSnapshot,
}

pub(super) struct CheckedNative {
    pub(super) plan: NativeSanitizerPlan,
    pub(super) inventories: [NativeInventory; 3],
    pub(super) total: usize,
}

fn reserve_directory(parent: &Path, identity: &str) -> io::Result<(String, PathBuf)> {
    for attempt in 0..32 {
        let identity = if attempt == 0 {
            identity.to_owned()
        } else {
            format!("{identity}-{attempt}")
        };
        let directory = parent.join(format!("rusterisk-native-sanitizers-{identity}"));
        match fs::create_dir(&directory) {
            Ok(()) => return Ok((identity, directory)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "native execution directory reservation limit exceeded",
    ))
}

impl CapturedNative {
    pub fn read(root: &Path) -> Result<Self, NativeError> {
        let source = SourceSnapshot::read(root)?;
        source.check(root)?;
        Ok(Self { source })
    }

    pub fn digest(&self) -> String {
        self.source.digest()
    }

    pub fn inputs(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.source.files()
    }

    pub fn run_with(
        &self,
        destination: &Path,
        image: &str,
        perform: impl FnMut(&mut Command, Duration, CommandMode) -> Result<(), ProcessError>,
    ) -> Result<usize, NativeError> {
        self.run_checked_with(destination, image, perform)
            .map(|checked| checked.total)
    }

    pub(super) fn run_checked_with(
        &self,
        destination: &Path,
        image: &str,
        mut perform: impl FnMut(&mut Command, Duration, CommandMode) -> Result<(), ProcessError>,
    ) -> Result<CheckedNative, NativeError> {
        if !destination.is_absolute() {
            return Err(io::Error::other("native result directory must be absolute").into());
        }
        fs::create_dir(destination)?;
        self.source.write(
            &destination.join("source"),
            &destination.join("captured-browser"),
        )?;
        write_new(
            &destination.join("SOURCE.sha256"),
            format!("{}\n", self.digest()).as_bytes(),
        )?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let (identity, directory) = reserve_directory(
            &std::env::temp_dir(),
            &format!("{}-{stamp}", std::process::id()),
        )?;
        let source = directory.join("source");
        let state = directory.join("state");
        let operation = (|| {
            self.source.write(&source, &state)?;
            for child in ["cargo", "target", "tmp", "native"] {
                fs::create_dir_all(state.join(child))?;
            }
            for defect in Defect::ALL {
                write_new(
                    &state.join(format!("probe-{}.c", defect.label())),
                    defect.source().as_bytes(),
                )?;
            }
            let plan = NativeSanitizerPlan::new(
                &identity,
                source
                    .to_str()
                    .ok_or_else(|| io::Error::other("native source path encoding"))?,
                state
                    .to_str()
                    .ok_or_else(|| io::Error::other("native state path encoding"))?,
                image,
            )?;
            let operation = (|| {
                for selected in [Phase::ClangVersion, Phase::RustVersion] {
                    phase(&plan, selected, &source, destination, &mut perform)?;
                }
                for defect in Defect::ALL {
                    phase(
                        &plan,
                        Phase::CompileProbe(defect),
                        &source,
                        destination,
                        &mut perform,
                    )?;
                    phase(
                        &plan,
                        Phase::RejectProbe(defect),
                        &source,
                        destination,
                        &mut perform,
                    )?;
                }
                phase(&plan, Phase::Fetch, &source, destination, &mut perform)?;
                for scope in Scope::ALL {
                    let bytes = phase(
                        &plan,
                        Phase::Build(scope),
                        &source,
                        destination,
                        &mut perform,
                    )?;
                    if scope == Scope::Tls {
                        for archive in native_archives(&bytes)? {
                            copy_archive(&archive, &state)?;
                            let selected = Phase::InspectArchive(archive.library());
                            let bytes = phase(&plan, selected, &source, destination, &mut perform)?;
                            validate_instrumented_archive(&bytes).map_err(|error| {
                                NativeError::Check {
                                    phase: selected,
                                    error,
                                }
                            })?;
                        }
                    }
                }
                let mut total = 0usize;
                let mut inventories = [None, None, None];
                for (scope, retained) in Scope::ALL.into_iter().zip(&mut inventories) {
                    let selected = Phase::Inventory(scope);
                    let bytes = phase(&plan, selected, &source, destination, &mut perform)?;
                    let inventory = NativeInventory::parse(scope, &bytes).map_err(|error| {
                        NativeError::Check {
                            phase: selected,
                            error,
                        }
                    })?;
                    let selected = Phase::Run(scope);
                    let bytes = phase(&plan, selected, &source, destination, &mut perform)?;
                    let count = inventory
                        .validate(&bytes)
                        .map_err(|error| NativeError::Check {
                            phase: selected,
                            error,
                        })?;
                    total = total
                        .checked_add(count)
                        .ok_or_else(|| io::Error::other("native test count overflow"))?;
                    *retained = Some(inventory);
                }
                let [Some(media), Some(storage), Some(tls)] = inventories else {
                    return Err(io::Error::other(
                        "native execution requires every scope inventory",
                    )
                    .into());
                };
                Ok(CheckedNative {
                    plan,
                    inventories: [media, storage, tls],
                    total,
                })
            })();
            follow(
                operation,
                self.source.check(&source).map_err(io::Error::other),
                "native execution source changed",
            )
        })();
        let operation = follow(
            operation,
            fs::remove_dir_all(directory),
            "native execution directory cleanup failed",
        );
        follow(
            operation,
            self.source
                .check(&destination.join("source"))
                .map_err(io::Error::other),
            "native preserved source changed",
        )
    }
}

#[cfg(test)]
mod tests;
