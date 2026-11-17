//! OCI verification binds one archive to native runtime and SDK checks.
//!
//! ```no_run
//! use std::path::Path;
//! use xtask::release_check::{
//!     CapturedReleaseSource, LinuxOciTarget, OciOutcome, OciRunError,
//! };
//! fn verify<'a>(
//!     captured: &'a CapturedReleaseSource,
//!     candidate: &Path,
//!     archive: &Path,
//!     sdk_candidate: &Path,
//! ) -> Result<OciOutcome<'a>, OciRunError> {
//!     captured.verify_oci(candidate, LinuxOciTarget::Arm64, archive, sdk_candidate)
//! }
//! ```
//!
//! OCI outcomes require native runtime execution.
//!
//! ```compile_fail
//! use xtask::release_check::{LinuxOciTarget, OciOutcome, RequiredGate};
//! fn planned<'a>() -> OciOutcome<'a> {
//!     RequiredGate::Oci(LinuxOciTarget::Arm64)
//! }
//! ```
//!
//! Private fields bind OCI outcomes to their captured source.
//!
//! ```compile_fail
//! use xtask::release_check::{CapturedReleaseSource, OciOutcome};
//! fn fabricated<'a>(
//!     mut outcome: OciOutcome<'a>,
//!     captured: &'a CapturedReleaseSource,
//! ) -> OciOutcome<'a> {
//!     outcome.captured = captured;
//!     outcome
//! }
//! ```
//!
//! Public OCI verification owns process execution.
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
//! ) -> Result<OciOutcome<'a>, OciRunError> {
//!     captured.verify_oci(
//!         candidate,
//!         LinuxOciTarget::Arm64,
//!         archive,
//!         sdk_candidate,
//!         |_: &mut Command, _: Duration| Ok::<(), ProcessError>(()),
//!     )
//! }
//! ```
//!
//! OCI verification requires a Linux target.
//!
//! ```compile_fail
//! use std::path::Path;
//! use xtask::{
//!     release_check::{CapturedReleaseSource, OciOutcome, OciRunError},
//!     server_distribution::ServerTarget,
//! };
//! fn non_linux<'a>(
//!     captured: &'a CapturedReleaseSource,
//!     candidate: &Path,
//!     archive: &Path,
//!     sdk_candidate: &Path,
//! ) -> Result<OciOutcome<'a>, OciRunError> {
//!     captured.verify_oci(candidate, ServerTarget::MacOsArm64, archive, sdk_candidate)
//! }
//! ```
