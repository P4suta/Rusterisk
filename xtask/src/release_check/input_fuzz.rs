use super::CapturedReleaseSource;
use crate::input_fuzz::{FuzzError, InputFuzzOutcome, LinuxTarget};
use std::path::Path;

#[cfg(unix)]
mod artifacts;

#[cfg(unix)]
fn read_bound_fuzz(
    captured: &CapturedReleaseSource,
    source: &Path,
) -> Result<crate::input_fuzz::CapturedInputFuzz, FuzzError> {
    captured.check(source).map_err(std::io::Error::other)?;
    let fuzz = crate::input_fuzz::CapturedInputFuzz::read(source)?;
    let inputs = super::SourceInputs::new(fuzz.inputs()).map_err(std::io::Error::other)?;
    captured
        .inputs()
        .and_then(|release| release.bind(&inputs))
        .map_err(std::io::Error::other)?;
    Ok(fuzz)
}

impl CapturedReleaseSource {
    /// Publishes all fuzz targets after native validation and complete owned cleanup.
    pub fn verify_input_fuzz_artifact(
        &self,
        image: &str,
        architecture: LinuxTarget,
        published: &Path,
    ) -> Result<InputFuzzOutcome, FuzzError> {
        #[cfg(unix)]
        {
            artifacts::produce(self, image, architecture, published)
        }
        #[cfg(not(unix))]
        {
            let _ = (image, architecture, published);
            Err(FuzzError::Process(
                crate::process::ProcessError::UnsupportedSupervision,
            ))
        }
    }

    /// Revalidates all retained fuzz targets, source inputs, tools, and seed bytes.
    pub fn read_input_fuzz_artifact(
        &self,
        directory: &Path,
        image: &str,
        architecture: LinuxTarget,
    ) -> Result<InputFuzzOutcome, FuzzError> {
        #[cfg(unix)]
        {
            artifacts::read(self, directory, image, architecture)
        }
        #[cfg(not(unix))]
        {
            let _ = (directory, image, architecture);
            Err(FuzzError::Process(
                crate::process::ProcessError::UnsupportedSupervision,
            ))
        }
    }
}
