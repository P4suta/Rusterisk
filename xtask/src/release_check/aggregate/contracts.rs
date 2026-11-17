//! Bind candidate completion to typed runner outcomes.
//!
//! ```no_run
//! use xtask::release_check::{
//!     CandidateCheckError, CandidateChecks, CandidateProfile, CapturedReleaseSource,
//!     CompletedCandidateChecks, RunnerOutcome,
//! };
//! fn complete<'source: 'run, 'run>(
//!     captured: &'source CapturedReleaseSource,
//!     profile: CandidateProfile,
//!     outcomes: [RunnerOutcome<'run, 'source>; 17],
//! ) -> Result<CompletedCandidateChecks<'run, 'source>, CandidateCheckError> {
//!     let mut checks = CandidateChecks::new(captured, profile)?;
//!     for outcome in outcomes {
//!         checks.include(outcome)?;
//!     }
//!     checks.finish()
//! }
//! ```
//!
//! Preserve the distinction between plans and completed outcomes.
//!
//! ```compile_fail
//! use xtask::release_check::{CandidateChecks, RequiredGate};
//! fn planned(checks: &mut CandidateChecks<'_, '_>) {
//!     checks.include(RequiredGate::Fast);
//! }
//! ```
//!
//! Reject success flags at the runner outcome boundary.
//!
//! ```compile_fail
//! use xtask::release_check::RunnerOutcome;
//! fn fabricated<'source: 'run, 'run>() -> RunnerOutcome<'run, 'source> {
//!     RunnerOutcome::Fast(true)
//! }
//! ```
//!
//! Keep all-target fuzz completion distinct from single-target completion.
//!
//! ```compile_fail
//! use xtask::{input_fuzz::InputFuzzTargetOutcome, release_check::RunnerOutcome};
//! fn partial<'source: 'run, 'run>(outcome: &'run InputFuzzTargetOutcome) -> RunnerOutcome<'run, 'source> {
//!     RunnerOutcome::InputFuzz(outcome)
//! }
//! ```
//!
//! Validate complete inventory before constructing the completed type.
//!
//! ```compile_fail
//! use xtask::release_check::{CandidateChecks, CompletedCandidateChecks};
//! fn unfinished<'source: 'run, 'run>(checks: CandidateChecks<'run, 'source>) -> CompletedCandidateChecks<'run, 'source> {
//!     checks.finish()
//! }
//! ```
//!
//! Keep completion fields private.
//!
//! ```compile_fail
//! use xtask::release_check::{CapturedReleaseSource, CompletedCandidateChecks};
//! fn fabricated<'source: 'run, 'run>(captured: &'source CapturedReleaseSource) -> CompletedCandidateChecks<'run, 'source> {
//!     CompletedCandidateChecks { captured, slots: [None; 17] }
//! }
//! ```
//!
//! Require a runner outcome alongside input identity.
//!
//! ```compile_fail
//! use xtask::release_check::{CandidateChecks, InputKey};
//! fn identity_only(checks: &mut CandidateChecks<'_, '_>, inputs: InputKey) {
//!     checks.include(inputs);
//! }
//! ```
