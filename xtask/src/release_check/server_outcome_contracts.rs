//! Server verification binds distributed bytes and executes the selected native target.
//!
//! ```no_run
//! use std::path::Path;
//! use xtask::{
//!     release_check::{CapturedReleaseSource, ServerOutcome, ServerRunError},
//!     server_distribution::ServerTarget,
//! };
//! fn verify<'a>(
//!     captured: &'a CapturedReleaseSource,
//!     candidate: &Path,
//!     target: ServerTarget,
//!     sdk_candidate: &Path,
//! ) -> Result<ServerOutcome<'a>, ServerRunError> {
//!     captured.verify_server(candidate, target, sdk_candidate)
//! }
//! ```
//!
//! Server outcomes require native execution.
//!
//! ```compile_fail
//! use xtask::{
//!     release_check::{RequiredGate, ServerOutcome},
//!     server_distribution::ServerTarget,
//! };
//! fn planned<'a>() -> ServerOutcome<'a> {
//!     RequiredGate::Server(ServerTarget::LinuxArm64)
//! }
//! ```
//!
//! Private fields bind server outcomes to their captured source.
//!
//! ```compile_fail
//! use xtask::release_check::{CapturedReleaseSource, ServerOutcome};
//! fn fabricated<'a>(
//!     mut outcome: ServerOutcome<'a>,
//!     captured: &'a CapturedReleaseSource,
//! ) -> ServerOutcome<'a> {
//!     outcome.captured = captured;
//!     outcome
//! }
//! ```
//!
//! Public server verification owns process execution.
//!
//! ```compile_fail
//! use std::{path::Path, process::Command, time::Duration};
//! use xtask::{
//!     process::ProcessError,
//!     release_check::{CapturedReleaseSource, ServerOutcome, ServerRunError},
//!     server_distribution::ServerTarget,
//! };
//! fn synthetic<'a>(
//!     captured: &'a CapturedReleaseSource,
//!     candidate: &Path,
//!     sdk_candidate: &Path,
//! ) -> Result<ServerOutcome<'a>, ServerRunError> {
//!     captured.verify_server(
//!         candidate,
//!         ServerTarget::LinuxArm64,
//!         sdk_candidate,
//!         |_: &mut Command, _: Duration| Ok::<(), ProcessError>(()),
//!     )
//! }
//! ```
