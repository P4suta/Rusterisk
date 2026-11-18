use super::CapturedReleaseSource;
use crate::core_mutation::{CoreMutationError, CoreMutationOutcome};
use std::path::Path;

#[cfg(unix)]
mod artifacts;

impl CapturedReleaseSource {
    /// Publishes native mutation outputs after input validation and owned cleanup.
    pub fn verify_core_mutation_artifact(
        &self,
        image: &str,
        destination: &Path,
    ) -> Result<CoreMutationOutcome, CoreMutationError> {
        #[cfg(unix)]
        {
            artifacts::produce(self, image, destination)
        }
        #[cfg(not(unix))]
        {
            let _ = (image, destination);
            Err(CoreMutationError::Process(
                crate::process::ProcessError::UnsupportedSupervision,
            ))
        }
    }

    /// Revalidates retained mutation inputs, phase exits, and native reports.
    pub fn read_core_mutation_artifact(
        &self,
        directory: &Path,
        image: &str,
    ) -> Result<CoreMutationOutcome, CoreMutationError> {
        #[cfg(unix)]
        {
            artifacts::read(self, directory, image)
        }
        #[cfg(not(unix))]
        {
            let _ = (directory, image);
            Err(CoreMutationError::Process(
                crate::process::ProcessError::UnsupportedSupervision,
            ))
        }
    }
}
