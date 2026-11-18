use super::CapturedReleaseSource;
use crate::{
    browser::{BrowserError, BrowserOutcome, RunError},
    reference::{ImageIdentity, ReferenceImage},
};
use std::{io, path::Path};

#[cfg(unix)]
mod artifacts;

impl CapturedReleaseSource {
    /// Publishes source-bound browser outputs after supervised execution and owned cleanup.
    pub fn verify_browser_artifact(
        &self,
        image: &str,
        destination: &Path,
    ) -> Result<BrowserOutcome, RunError> {
        let image = ImageIdentity::parse(image).ok_or(BrowserError::Plan)?;
        #[cfg(unix)]
        {
            artifacts::produce(self, image.as_str(), None, destination)
        }
        #[cfg(not(unix))]
        {
            let _ = (image, destination);
            Err(RunError::Process(
                crate::process::ProcessError::UnsupportedSupervision,
            ))
        }
    }

    /// Revalidates retained browser inputs, native reports, received audio, and tool versions.
    pub fn read_browser_artifact(
        &self,
        directory: &Path,
        image: &str,
    ) -> Result<BrowserOutcome, RunError> {
        let image = ImageIdentity::parse(image).ok_or(BrowserError::Plan)?;
        #[cfg(unix)]
        {
            artifacts::read(self, directory, image.as_str(), None)
        }
        #[cfg(not(unix))]
        {
            let _ = (image, directory);
            Err(RunError::Process(
                crate::process::ProcessError::UnsupportedSupervision,
            ))
        }
    }

    pub fn verify_browser_comparison_artifact(
        &self,
        image: &str,
        reference: &str,
        destination: &Path,
    ) -> Result<BrowserOutcome, RunError> {
        let image = ImageIdentity::parse(image).ok_or(BrowserError::Plan)?;
        let reference = ReferenceImage::parse_inspection(reference).map_err(io::Error::other)?;
        #[cfg(unix)]
        {
            artifacts::produce(self, image.as_str(), Some(reference.as_str()), destination)
        }
        #[cfg(not(unix))]
        {
            let _ = (image, reference, destination);
            Err(RunError::Process(
                crate::process::ProcessError::UnsupportedSupervision,
            ))
        }
    }

    pub fn read_browser_comparison_artifact(
        &self,
        directory: &Path,
        image: &str,
        reference: &str,
    ) -> Result<BrowserOutcome, RunError> {
        let image = ImageIdentity::parse(image).ok_or(BrowserError::Plan)?;
        let reference = ReferenceImage::parse_inspection(reference).map_err(io::Error::other)?;
        #[cfg(unix)]
        {
            artifacts::read(self, directory, image.as_str(), Some(reference.as_str()))
        }
        #[cfg(not(unix))]
        {
            let _ = (image, reference, directory);
            Err(RunError::Process(
                crate::process::ProcessError::UnsupportedSupervision,
            ))
        }
    }
}
