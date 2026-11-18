//! OCI artifact production owns native execution and publishes after cleanup.
//!
//! ```no_run
//! use std::path::Path;
//! use xtask::release_check::{
//!     CapturedReleaseSource, LinuxOciTarget, OciOutcome, OciRunError,
//! };
//! fn produce<'a>(
//!     captured: &'a CapturedReleaseSource,
//!     candidate: &Path,
//!     archive: &Path,
//!     sdk_candidate: &Path,
//!     published: &Path,
//! ) -> Result<OciOutcome<'a>, OciRunError> {
//!     captured.verify_oci_artifact(
//!         candidate, LinuxOciTarget::Arm64, archive, sdk_candidate, published,
//!     )
//! }
//! ```
//!
//! OCI artifact readers revalidate source, candidates, and native runtime outputs.
//!
//! ```no_run
//! use std::path::Path;
//! use xtask::release_check::{
//!     CapturedReleaseSource, LinuxOciTarget, OciOutcome, OciRunError,
//! };
//! fn read<'a>(
//!     captured: &'a CapturedReleaseSource,
//!     artifact: &Path,
//!     candidate: &Path,
//!     archive: &Path,
//!     sdk_candidate: &Path,
//! ) -> Result<OciOutcome<'a>, OciRunError> {
//!     captured.read_oci_artifact(
//!         artifact, candidate, LinuxOciTarget::Arm64, archive, sdk_candidate,
//!     )
//! }
//! ```
//!
//! Public OCI artifact production keeps its supervisor concrete.
//!
//! ```compile_fail
//! use std::{path::Path, process::Command, time::Duration};
//! use xtask::{
//!     process::ProcessError,
//!     release_check::{CapturedReleaseSource, LinuxOciTarget, OciOutcome, OciRunError},
//! };
//! fn synthetic<'a>(
//!     captured: &'a CapturedReleaseSource,
//!     candidate: &Path,
//!     archive: &Path,
//!     sdk_candidate: &Path,
//!     published: &Path,
//! ) -> Result<OciOutcome<'a>, OciRunError> {
//!     captured.verify_oci_artifact(
//!         candidate,
//!         LinuxOciTarget::Arm64,
//!         archive,
//!         sdk_candidate,
//!         published,
//!         |_: &mut Command, _: Duration| Ok::<(), ProcessError>(()),
//!     )
//! }
//! ```
//!
//! OCI artifact readers construct opaque outcomes through native validation.
//!
//! ```compile_fail
//! use xtask::release_check::OciOutcome;
//! fn fabricated(mut outcome: OciOutcome<'_>) -> OciOutcome<'_> {
//!     outcome.archive_checksum = "unverified archive".to_owned();
//!     outcome
//! }
//! ```
