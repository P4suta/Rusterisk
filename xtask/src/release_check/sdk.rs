use super::{CapturedReleaseSource, InputError, InputKey, SourceInputs};
use crate::{
    distribution::{
        verify_sdk_consumers_supervised, CapturedSdk, SdkConsumerRunError, SdkContents,
        SdkContentsError, SdkPackage, SdkPort,
    },
    process::{CommandSupervisor, ProcessError},
};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub struct SdkOutcome<'a> {
    captured: &'a CapturedReleaseSource,
    projection: InputKey,
    version: String,
    checksums: [String; 3],
}

impl SdkOutcome<'_> {
    pub fn inputs(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.captured.files()
    }

    pub const fn projection(&self) -> InputKey {
        self.projection
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn checksum(&self, package: SdkPackage) -> &str {
        let [core, api, client] = &self.checksums;
        match package {
            SdkPackage::Core => core,
            SdkPackage::Api => api,
            SdkPackage::Client => client,
        }
    }
}

enum Failure {
    Io(io::Error),
    Contents(SdkContentsError),
    Inputs(InputError),
    Consumer(SdkConsumerRunError),
    Process(ProcessError),
}

enum Diagnostic {
    Io(String),
    Contents(String),
    Inputs(String),
    Consumer(String),
    Process(String),
}

pub struct SdkRunError {
    preceding: Option<String>,
    stage: &'static str,
    failure: Diagnostic,
}

impl SdkRunError {
    fn new(stage: &'static str, failure: Failure) -> Self {
        let failure = match failure {
            Failure::Io(error) => Diagnostic::Io(error.to_string()),
            Failure::Contents(error) => Diagnostic::Contents(error.to_string()),
            Failure::Inputs(error) => Diagnostic::Inputs(error.to_string()),
            Failure::Consumer(error) => Diagnostic::Consumer(error.to_string()),
            Failure::Process(error) => Diagnostic::Process(error.to_string()),
        };
        Self {
            preceding: None,
            stage,
            failure,
        }
    }
}

impl std::fmt::Debug for SdkRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

impl std::fmt::Display for SdkRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(preceding) = &self.preceding {
            write!(formatter, "{preceding}; ")?;
        }
        write!(formatter, "{}: ", self.stage)?;
        match &self.failure {
            Diagnostic::Io(cause)
            | Diagnostic::Contents(cause)
            | Diagnostic::Inputs(cause)
            | Diagnostic::Consumer(cause)
            | Diagnostic::Process(cause) => formatter.write_str(cause),
        }
    }
}

impl std::error::Error for SdkRunError {}

fn combine<T>(
    operation: Result<T, SdkRunError>,
    follow_up: Result<(), SdkRunError>,
) -> Result<T, SdkRunError> {
    match (operation, follow_up) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(mut follow_up)) => {
            follow_up.preceding = Some(match follow_up.preceding.take() {
                Some(preceding) => format!("{operation}; {preceding}"),
                None => operation.to_string(),
            });
            Err(follow_up)
        }
    }
}

fn load(
    captured: &CapturedReleaseSource,
    candidate: &Path,
) -> Result<(SdkContents, InputKey), SdkRunError> {
    load_paired(captured, candidate).map(|(_, contents, projection)| (contents, projection))
}

fn load_paired(
    captured: &CapturedReleaseSource,
    candidate: &Path,
) -> Result<(CapturedSdk, SdkContents, InputKey), SdkRunError> {
    let (source, contents) = SdkContents::read_paired(candidate)
        .map_err(|error| SdkRunError::new("SDK candidate contents", Failure::Contents(error)))?;
    let projection = SourceInputs::new(source.files())
        .map_err(|error| SdkRunError::new("SDK candidate projection", Failure::Inputs(error)))?;
    let release = captured
        .inputs()
        .map_err(|error| SdkRunError::new("SDK release inputs", Failure::Inputs(error)))?;
    let projection = release
        .bind(&projection)
        .map_err(|error| SdkRunError::new("SDK source binding", Failure::Inputs(error)))?;
    Ok((source, contents, projection))
}

fn verify_sdk_with<'a>(
    captured: &'a CapturedReleaseSource,
    candidate: &Path,
    execution: &Path,
    artifacts: &Path,
    port: impl SdkPort,
) -> Result<SdkOutcome<'a>, SdkRunError> {
    let loaded = load(captured, candidate).and_then(|loaded| {
        announce(&loaded.0, artifacts)
            .map_err(|error| SdkRunError::new("SDK artifact output", Failure::Io(error)))?;
        Ok(loaded)
    });
    let (contents, projection) = match loaded {
        Ok(loaded) => loaded,
        Err(error) => {
            return combine(
                Err(error),
                port.finish().map_err(|error| {
                    SdkRunError::new("SDK supervisor cleanup", Failure::Process(error))
                }),
            )
        }
    };
    verify_sdk_consumers_supervised(&contents, execution, artifacts, port)
        .map_err(|error| SdkRunError::new("SDK native consumers", Failure::Consumer(error)))?;
    Ok(SdkOutcome {
        captured,
        projection,
        version: contents.version().to_owned(),
        checksums: SdkPackage::ALL.map(|package| contents.checksum(package).to_owned()),
    })
}

fn announce(contents: &SdkContents, artifacts: &Path) -> io::Result<()> {
    let mut output = io::stdout();
    writeln!(output, "SDK consumer artifacts: {}", artifacts.display())?;
    writeln!(output, "SDK consumer source: {}", contents.source_digest())?;
    output.flush()
}

struct Paths {
    candidate: PathBuf,
    execution: PathBuf,
    artifacts: PathBuf,
}

impl Paths {
    fn new(candidate: &Path) -> Result<Self, SdkRunError> {
        let candidate = std::path::absolute(candidate)
            .map_err(|error| SdkRunError::new("SDK candidate path", Failure::Io(error)))?;
        let parent = candidate.parent().ok_or_else(|| {
            SdkRunError::new(
                "SDK candidate parent",
                Failure::Io(io::Error::other("SDK candidate requires a physical parent")),
            )
        })?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| {
                SdkRunError::new("SDK execution clock", Failure::Io(io::Error::other(error)))
            })?
            .as_nanos();
        let identity = format!("{}-{stamp}", std::process::id());
        let artifacts = parent.join(format!("sdk-consumers-{identity}"));
        let execution = fs::canonicalize(std::env::temp_dir())
            .map_err(|error| SdkRunError::new("SDK temporary root", Failure::Io(error)))?
            .join(format!("rusterisk-sdk-consumers-{identity}"));
        Ok(Self {
            candidate,
            execution,
            artifacts,
        })
    }
}

impl CapturedReleaseSource {
    /// Runs every SDK consumer from source-bound candidate archive bytes.
    /// Returns only after native validation and owned process and workspace cleanup.
    pub fn verify_sdk(&self, candidate: &Path) -> Result<SdkOutcome<'_>, SdkRunError> {
        let paths = Paths::new(candidate)?;
        let supervisor = CommandSupervisor::new()
            .map_err(|error| SdkRunError::new("SDK native supervisor", Failure::Process(error)))?;
        verify_sdk_with(
            self,
            &paths.candidate,
            &paths.execution,
            &paths.artifacts,
            supervisor,
        )
    }
}

#[cfg(any(test, doctest))]
#[path = "sdk_outcome_contracts.rs"]
mod contracts;

#[cfg(test)]
#[path = "sdk_tests.rs"]
mod tests;
