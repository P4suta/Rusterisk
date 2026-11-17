//! SDK verification loads a candidate and runs the maintained native consumers.
//!
//! ```no_run
//! use std::path::Path;
//! use xtask::release_check::{CapturedReleaseSource, SdkOutcome, SdkRunError};
//! fn verify<'a>(
//!     captured: &'a CapturedReleaseSource,
//!     candidate: &Path,
//! ) -> Result<SdkOutcome<'a>, SdkRunError> {
//!     captured.verify_sdk(candidate)
//! }
//! ```
//!
//! Completed verification constructs SDK outcomes.
//!
//! ```compile_fail
//! use xtask::release_check::{RequiredGate, SdkOutcome};
//! fn planned<'a>() -> SdkOutcome<'a> {
//!     RequiredGate::Sdk
//! }
//! ```
//!
//! SDK outcomes keep their source fields private.
//!
//! ```compile_fail
//! use xtask::release_check::{CapturedReleaseSource, SdkOutcome};
//! fn fabricated<'a>(
//!     mut outcome: SdkOutcome<'a>,
//!     captured: &'a CapturedReleaseSource,
//! ) -> SdkOutcome<'a> {
//!     outcome.captured = captured;
//!     outcome
//! }
//! ```
//!
//! Public SDK verification owns its process supervisor.
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
//! ) -> Result<SdkOutcome<'a>, SdkRunError> {
//!     captured.verify_sdk(candidate, |_: &mut Command, _: Duration| {
//!         Ok::<(), ProcessError>(())
//!     })
//! }
//! ```
