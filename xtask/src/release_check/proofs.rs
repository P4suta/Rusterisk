use super::CapturedReleaseSource;
use crate::core_proofs::{CoreProofOutcome, ProofError};

#[cfg(unix)]
mod native;

impl CapturedReleaseSource {
    /// Publishes native Kani results after source checks and complete owned cleanup.
    pub fn verify_core_proofs_artifact(
        &self,
        image: &str,
        destination: &std::path::Path,
    ) -> Result<CoreProofOutcome, ProofError> {
        #[cfg(unix)]
        {
            native::produce(self, image, destination)
        }
        #[cfg(not(unix))]
        {
            let _ = (image, destination);
            Err(ProofError::Process(
                crate::process::ProcessError::UnsupportedSupervision,
            ))
        }
    }

    /// Revalidates retained Kani results and their full captured inputs.
    pub fn read_core_proofs_artifact(
        &self,
        directory: &std::path::Path,
        image: &str,
    ) -> Result<CoreProofOutcome, ProofError> {
        #[cfg(unix)]
        {
            native::read(self, directory, image)
        }
        #[cfg(not(unix))]
        {
            let _ = (directory, image);
            Err(ProofError::Process(
                crate::process::ProcessError::UnsupportedSupervision,
            ))
        }
    }
}
