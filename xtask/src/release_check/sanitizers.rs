use super::CapturedReleaseSource;
use crate::native_sanitizers::{NativeError, NativeOutcome};
use std::path::Path;

#[cfg(unix)]
mod artifacts;

#[cfg(unix)]
fn read_bound_native(
    captured: &CapturedReleaseSource,
    source: &Path,
) -> Result<crate::native_sanitizers::CapturedNative, NativeError> {
    use super::SourceInputs;
    captured.check(source).map_err(std::io::Error::other)?;
    let native = crate::native_sanitizers::CapturedNative::read(source)?;
    let projection = SourceInputs::new(native.inputs()).map_err(std::io::Error::other)?;
    captured
        .inputs()
        .and_then(|release| release.bind(&projection))
        .map_err(std::io::Error::other)?;
    Ok(native)
}

impl CapturedReleaseSource {
    /// Publishes native sanitizer reports after supervised execution and owned cleanup.
    pub fn verify_native_sanitizers_artifact(
        &self,
        image: &str,
        published: &Path,
    ) -> Result<NativeOutcome, NativeError> {
        #[cfg(unix)]
        {
            artifacts::produce(self, image, published)
        }
        #[cfg(not(unix))]
        {
            let _ = (image, published);
            Err(std::io::Error::other(crate::process::ProcessError::UnsupportedSupervision).into())
        }
    }

    /// Revalidates native sanitizer sources, compiler reports, probes, and complete scope results.
    pub fn read_native_sanitizers_artifact(
        &self,
        directory: &Path,
        image: &str,
    ) -> Result<NativeOutcome, NativeError> {
        #[cfg(unix)]
        {
            artifacts::read(self, directory, image)
        }
        #[cfg(not(unix))]
        {
            let _ = (directory, image);
            Err(std::io::Error::other(crate::process::ProcessError::UnsupportedSupervision).into())
        }
    }
}
