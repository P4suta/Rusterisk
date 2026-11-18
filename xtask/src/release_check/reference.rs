use super::CapturedReleaseSource;
#[cfg(unix)]
mod artifacts;
use crate::{
    process::ProcessError,
    reference::{ReferenceImage, ReferenceRunner},
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// Binds native reference execution to captured inputs and one immutable image.
///
/// ```no_run
/// use xtask::{
///     reference::ReferenceImage,
///     release_check::{CapturedReleaseSource, ReferenceOutcome, ReferenceRunError},
/// };
/// fn verify<'a>(
///     captured: &'a CapturedReleaseSource,
///     image: &'a ReferenceImage,
/// ) -> Result<ReferenceOutcome<'a>, ReferenceRunError> {
///     captured.verify_reference(image)
/// }
/// ```
pub struct ReferenceOutcome<'a> {
    captured: &'a CapturedReleaseSource,
    image: &'a ReferenceImage,
}

impl ReferenceOutcome<'_> {
    pub fn inputs(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.captured.files()
    }

    pub const fn image(&self) -> &ReferenceImage {
        self.image
    }
}

enum Failure {
    Io(io::Error),
    Source(String),
    Process(ProcessError),
    Contract(&'static str),
}

/// Preserves comparison and cleanup failures in their observed order.
pub struct ReferenceRunError {
    preceding: Option<String>,
    stage: &'static str,
    failure: Failure,
}

impl ReferenceRunError {
    fn new(stage: &'static str, failure: Failure) -> Self {
        Self {
            preceding: None,
            stage,
            failure,
        }
    }
}

impl std::fmt::Debug for ReferenceRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

impl std::fmt::Display for ReferenceRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(preceding) = &self.preceding {
            write!(formatter, "{preceding}; ")?;
        }
        write!(formatter, "{}: ", self.stage)?;
        match &self.failure {
            Failure::Io(error) => error.fmt(formatter),
            Failure::Source(diagnostic) => formatter.write_str(diagnostic),
            Failure::Process(error) => error.fmt(formatter),
            Failure::Contract(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for ReferenceRunError {}

fn combine<T>(
    operation: Result<T, ReferenceRunError>,
    follow_up: Result<(), ReferenceRunError>,
) -> Result<T, ReferenceRunError> {
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

trait ReferencePort {
    fn inspect_image(
        &mut self,
        source: &Path,
        image: &ReferenceImage,
    ) -> Result<String, ProcessError>;
    fn compare(&mut self, source: &Path, image: &ReferenceImage) -> Result<(), ProcessError>;
    fn finish(self) -> Result<(), ProcessError>;
}

impl ReferencePort for ReferenceRunner {
    fn inspect_image(
        &mut self,
        source: &Path,
        image: &ReferenceImage,
    ) -> Result<String, ProcessError> {
        ReferenceRunner::inspect_image(self, source, image)
    }

    fn compare(&mut self, source: &Path, image: &ReferenceImage) -> Result<(), ProcessError> {
        ReferenceRunner::compare(self, source, image)
    }

    fn finish(self) -> Result<(), ProcessError> {
        ReferenceRunner::finish(self)
    }
}

struct Workspace {
    root: PathBuf,
    source: PathBuf,
    state: PathBuf,
}

impl Workspace {
    fn create(directory: &Path) -> Result<Self, ReferenceRunError> {
        fs::create_dir(directory)
            .map_err(|error| ReferenceRunError::new("reference workspace", Failure::Io(error)))?;
        Ok(Self {
            root: directory.to_owned(),
            source: directory.join("source"),
            state: directory.join("state"),
        })
    }
}

fn verify_reference_with<'a>(
    captured: &'a CapturedReleaseSource,
    image: &'a ReferenceImage,
    directory: &Path,
    mut port: impl ReferencePort,
) -> Result<ReferenceOutcome<'a>, ReferenceRunError> {
    let workspace = match Workspace::create(directory) {
        Ok(workspace) => workspace,
        Err(error) => {
            return combine(
                Err(error),
                port.finish().map_err(|error| {
                    ReferenceRunError::new("reference supervisor cleanup", Failure::Process(error))
                }),
            );
        }
    };
    let operation = (|| {
        captured
            .write(&workspace.source, &workspace.state)
            .map_err(|error| {
                ReferenceRunError::new("reference source clone", Failure::Source(error.to_string()))
            })?;
        let inspected = port
            .inspect_image(&workspace.source, image)
            .map_err(|error| {
                ReferenceRunError::new("reference image inspection", Failure::Process(error))
            })?;
        let inspected = ReferenceImage::parse_inspection(&inspected).map_err(|_| {
            ReferenceRunError::new(
                "reference image inspection",
                Failure::Contract("reference image inspection must return an immutable image ID"),
            )
        })?;
        if inspected.as_str() != image.as_str() {
            return Err(ReferenceRunError::new(
                "reference image inspection",
                Failure::Contract("reference image differs from the selected image"),
            ));
        }
        port.compare(&workspace.source, image).map_err(|error| {
            ReferenceRunError::new("reference comparisons", Failure::Process(error))
        })
    })();
    let operation = combine(
        operation,
        port.finish().map_err(|error| {
            ReferenceRunError::new("reference supervisor cleanup", Failure::Process(error))
        }),
    );
    let operation = combine(
        operation,
        captured.check(&workspace.source).map_err(|error| {
            ReferenceRunError::new(
                "captured reference source check",
                Failure::Source(error.to_string()),
            )
        }),
    );
    combine(
        operation,
        fs::remove_dir_all(&workspace.root).map_err(|error| {
            ReferenceRunError::new("owned reference state cleanup", Failure::Io(error))
        }),
    )?;
    Ok(ReferenceOutcome { captured, image })
}

impl CapturedReleaseSource {
    /// Publishes reference native results after complete owned cleanup.
    pub fn verify_reference_artifact<'a>(
        &'a self,
        image: &'a ReferenceImage,
        published: &Path,
    ) -> Result<ReferenceOutcome<'a>, ReferenceRunError> {
        #[cfg(unix)]
        {
            artifacts::produce(self, image, published)
        }
        #[cfg(not(unix))]
        {
            let _ = (image, published);
            Err(ReferenceRunError::new(
                "reference artifact supervisor",
                Failure::Process(ProcessError::UnsupportedSupervision),
            ))
        }
    }

    /// Revalidates reference native outputs with the captured source and image.
    pub fn read_reference_artifact<'a>(
        &'a self,
        artifact: &Path,
        image: &'a ReferenceImage,
    ) -> Result<ReferenceOutcome<'a>, ReferenceRunError> {
        #[cfg(unix)]
        {
            artifacts::read(self, image, artifact)
        }
        #[cfg(not(unix))]
        {
            let _ = (artifact, image);
            Err(ReferenceRunError::new(
                "reference artifact reader",
                Failure::Process(ProcessError::UnsupportedSupervision),
            ))
        }
    }

    /// Runs every maintained reference comparison from captured inputs.
    /// Releases supervised processes and owned Docker resources before returning.
    pub fn verify_reference<'a>(
        &'a self,
        image: &'a ReferenceImage,
    ) -> Result<ReferenceOutcome<'a>, ReferenceRunError> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| {
                ReferenceRunError::new(
                    "reference execution clock",
                    Failure::Io(io::Error::other(error)),
                )
            })?
            .as_nanos();
        let directory = fs::canonicalize(std::env::temp_dir())
            .map_err(|error| {
                ReferenceRunError::new("reference temporary root", Failure::Io(error))
            })?
            .join(format!(
                "rusterisk-reference-{}-{stamp}",
                std::process::id()
            ));
        let runner = ReferenceRunner::new(directory.join("state")).map_err(|error| {
            ReferenceRunError::new("reference supervisor", Failure::Process(error))
        })?;
        verify_reference_with(self, image, &directory, runner)
    }
}

#[cfg(any(test, doctest))]
#[path = "reference_outcome_contracts.rs"]
mod contracts;

#[cfg(any(test, doctest))]
#[path = "reference_artifact_contracts.rs"]
mod artifact_contracts;

#[cfg(test)]
#[path = "reference_outcome_tests.rs"]
mod tests;
