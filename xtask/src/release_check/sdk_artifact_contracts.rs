//! SDK artifact production owns native execution and publishes after cleanup.
//!
//! ```no_run
//! use std::path::Path;
//! use xtask::release_check::{CapturedReleaseSource, SdkOutcome, SdkRunError};
//! fn produce<'a>(
//!     captured: &'a CapturedReleaseSource,
//!     candidate: &Path,
//!     published: &Path,
//! ) -> Result<SdkOutcome<'a>, SdkRunError> {
//!     captured.verify_sdk_artifact(candidate, published)
//! }
//! ```
//!
//! SDK artifact readers revalidate retained inputs and native outputs.
//!
//! ```no_run
//! use std::path::Path;
//! use xtask::release_check::{CapturedReleaseSource, SdkOutcome, SdkRunError};
//! fn read<'a>(
//!     captured: &'a CapturedReleaseSource,
//!     artifact: &Path,
//!     candidate: &Path,
//! ) -> Result<SdkOutcome<'a>, SdkRunError> {
//!     captured.read_sdk_artifact(artifact, candidate)
//! }
//! ```
//!
//! Public SDK artifact production keeps its supervisor concrete.
//!
//! ```compile_fail
//! use std::{path::Path, process::Command, time::Duration};
//! use xtask::{
//!     process::ProcessError,
//!     release_check::{CapturedReleaseSource, SdkOutcome, SdkRunError},
//! };
//! fn synthetic<'a>(
//!     captured: &'a CapturedReleaseSource,
//!     candidate: &Path,
//!     published: &Path,
//! ) -> Result<SdkOutcome<'a>, SdkRunError> {
//!     captured.verify_sdk_artifact(candidate, published, |_: &mut Command, _: Duration| {
//!         Ok::<(), ProcessError>(())
//!     })
//! }
//! ```
//!
//! SDK artifact readers construct opaque outcomes through native validation.
//!
//! ```compile_fail
//! use xtask::release_check::{SdkOutcome, SourceInputs};
//! fn fabricated<'a>(
//!     mut outcome: SdkOutcome<'a>,
//!     inputs: &SourceInputs,
//! ) -> SdkOutcome<'a> {
//!     outcome.projection = inputs.key();
//!     outcome
//! }
//! ```
