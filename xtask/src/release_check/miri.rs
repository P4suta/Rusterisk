use super::CapturedReleaseSource;
use crate::core_miri::{CoreMiriError, CoreMiriOutcome};
use std::path::Path;

#[cfg(unix)]
mod artifacts;

#[cfg(unix)]
fn read_bound_miri(
    captured: &CapturedReleaseSource,
    source: &Path,
) -> Result<crate::core_miri::CapturedCoreMiri, CoreMiriError> {
    use super::SourceInputs;
    captured.check(source).map_err(std::io::Error::other)?;
    let miri = crate::core_miri::CapturedCoreMiri::read(source)?;
    let inputs = SourceInputs::new(miri.inputs()).map_err(std::io::Error::other)?;
    captured
        .inputs()
        .and_then(|release| release.bind(&inputs))
        .map_err(std::io::Error::other)?;
    Ok(miri)
}

impl CapturedReleaseSource {
    /// Publishes Miri native results after supervised execution and owned cleanup.
    pub fn verify_core_miri_artifact(
        &self,
        image: &str,
        published: &Path,
    ) -> Result<CoreMiriOutcome, CoreMiriError> {
        #[cfg(unix)]
        {
            artifacts::produce(self, image, published)
        }
        #[cfg(not(unix))]
        {
            let _ = (image, published);
            Err(CoreMiriError::Process(
                crate::process::ProcessError::UnsupportedSupervision,
            ))
        }
    }

    /// Revalidates retained Miri source, native reports, and tool versions.
    pub fn read_core_miri_artifact(
        &self,
        directory: &Path,
        image: &str,
    ) -> Result<CoreMiriOutcome, CoreMiriError> {
        #[cfg(unix)]
        {
            artifacts::read(self, directory, image)
        }
        #[cfg(not(unix))]
        {
            let _ = (directory, image);
            Err(CoreMiriError::Process(
                crate::process::ProcessError::UnsupportedSupervision,
            ))
        }
    }
}

#[cfg(test)]
#[path = "miri_tests.rs"]
mod tests;

#[cfg(any(test, doctest))]
#[path = "miri_artifact_contracts.rs"]
mod contracts;
