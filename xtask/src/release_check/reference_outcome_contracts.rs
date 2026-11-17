//! A reference plan cannot satisfy native reference verification.
//!
//! ```compile_fail
//! use xtask::release_check::{ReferenceOutcome, RequiredGate};
//! fn planned<'a>() -> ReferenceOutcome<'a> {
//!     RequiredGate::Reference
//! }
//! ```
//!
//! Reference outcome construction remains private.
//!
//! ```compile_fail
//! use xtask::{
//!     reference::ReferenceImage,
//!     release_check::{CapturedReleaseSource, ReferenceOutcome},
//! };
//! fn fabricated<'a>(
//!     captured: &'a CapturedReleaseSource,
//!     image: &'a ReferenceImage,
//! ) -> ReferenceOutcome<'a> {
//!     ReferenceOutcome { captured, image }
//! }
//! ```
//!
//! Public verification accepts an image without a supplied process performer.
//!
//! ```compile_fail
//! use xtask::{
//!     reference::ReferenceImage,
//!     release_check::{CapturedReleaseSource, ReferenceOutcome},
//! };
//! fn synthetic<'a>(
//!     captured: &'a CapturedReleaseSource,
//!     image: &'a ReferenceImage,
//! ) -> ReferenceOutcome<'a> {
//!     captured.verify_reference_with(image, || Ok::<(), std::io::Error>(()))
//! }
//! ```
