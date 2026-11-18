//! Miri artifact production owns native execution and publishes after cleanup.
//!
//! ```no_run
//! use std::path::Path;
//! use xtask::{
//!     core_miri::{CoreMiriError, CoreMiriOutcome},
//!     release_check::CapturedReleaseSource,
//! };
//! fn produce(
//!     captured: &CapturedReleaseSource,
//!     image: &str,
//!     published: &Path,
//! ) -> Result<CoreMiriOutcome, CoreMiriError> {
//!     captured.verify_core_miri_artifact(image, published)
//! }
//! ```
//!
//! Miri artifact readers revalidate retained source, tools, and native inventory.
//!
//! ```no_run
//! use std::path::Path;
//! use xtask::{
//!     core_miri::{CoreMiriError, CoreMiriOutcome},
//!     release_check::CapturedReleaseSource,
//! };
//! fn read(
//!     captured: &CapturedReleaseSource,
//!     directory: &Path,
//!     image: &str,
//! ) -> Result<CoreMiriOutcome, CoreMiriError> {
//!     captured.read_core_miri_artifact(directory, image)
//! }
//! ```
//!
//! Public Miri artifact production keeps its supervisor concrete.
//!
//! ```compile_fail
//! use std::{path::Path, process::Command, time::Duration};
//! use xtask::{
//!     core_miri::{CoreMiriError, CoreMiriOutcome},
//!     process::ProcessError,
//!     release_check::CapturedReleaseSource,
//! };
//! fn synthetic(
//!     captured: &CapturedReleaseSource,
//!     image: &str,
//!     published: &Path,
//! ) -> Result<CoreMiriOutcome, CoreMiriError> {
//!     captured.verify_core_miri_artifact(image, published, |_: &mut Command, _: Duration| {
//!         Ok::<(), ProcessError>(())
//!     })
//! }
//! ```
//!
//! Miri outcomes keep their execution image private.
//!
//! ```compile_fail
//! use xtask::core_miri::CoreMiriOutcome;
//! fn fabricated(outcome: CoreMiriOutcome) -> CoreMiriOutcome {
//!     let _ = &outcome.image;
//!     outcome
//! }
//! ```
