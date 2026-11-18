//! Reference artifacts keep native validation behind the captured-source boundary.
//!
//! ```no_run
//! use std::path::Path;
//! use xtask::{
//!     reference::ReferenceImage,
//!     release_check::{CapturedReleaseSource, ReferenceOutcome, ReferenceRunError},
//! };
//! fn produce<'a>(
//!     captured: &'a CapturedReleaseSource,
//!     image: &'a ReferenceImage,
//!     published: &Path,
//! ) -> Result<ReferenceOutcome<'a>, ReferenceRunError> {
//!     captured.verify_reference_artifact(image, published)
//! }
//! ```
//!
//! ```no_run
//! use std::path::Path;
//! use xtask::{
//!     reference::ReferenceImage,
//!     release_check::{CapturedReleaseSource, ReferenceOutcome, ReferenceRunError},
//! };
//! fn read<'a>(
//!     captured: &'a CapturedReleaseSource,
//!     image: &'a ReferenceImage,
//!     artifact: &Path,
//! ) -> Result<ReferenceOutcome<'a>, ReferenceRunError> {
//!     captured.read_reference_artifact(artifact, image)
//! }
//! ```
