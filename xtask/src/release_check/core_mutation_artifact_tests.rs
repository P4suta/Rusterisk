//! Mutation artifact production owns native execution and publication.
//!
//! ```no_run
//! use std::path::Path;
//! use xtask::{
//!     core_mutation::{CoreMutationError, CoreMutationOutcome},
//!     release_check::CapturedReleaseSource,
//! };
//! fn produce(
//!     captured: &CapturedReleaseSource,
//!     image: &str,
//!     destination: &Path,
//! ) -> Result<CoreMutationOutcome, CoreMutationError> {
//!     captured.verify_core_mutation_artifact(image, destination)
//! }
//! ```
//!
//! Mutation artifact readers validate retained native inputs and outputs.
//!
//! ```no_run
//! use std::path::Path;
//! use xtask::{
//!     core_mutation::{CoreMutationError, CoreMutationOutcome},
//!     release_check::CapturedReleaseSource,
//! };
//! fn read(
//!     captured: &CapturedReleaseSource,
//!     directory: &Path,
//!     image: &str,
//! ) -> Result<CoreMutationOutcome, CoreMutationError> {
//!     captured.read_core_mutation_artifact(directory, image)
//! }
//! ```
//!
//! Callers can't replace supervised execution with a success callback.
//!
//! ```compile_fail
//! use std::{path::Path, process::Command, time::Duration};
//! use xtask::{
//!     core_mutation::{CoreMutationError, CoreMutationOutcome},
//!     process::ProcessError,
//!     release_check::CapturedReleaseSource,
//! };
//! fn synthetic(
//!     captured: &CapturedReleaseSource,
//!     image: &str,
//!     destination: &Path,
//! ) -> Result<CoreMutationOutcome, CoreMutationError> {
//!     captured.verify_core_mutation_artifact(image, destination, |_: &mut Command, _: Duration| {
//!         Ok::<(), ProcessError>(())
//!     })
//! }
//! ```
//!
//! Readers accept retained paths without a supplied performer.
//!
//! ```compile_fail
//! use std::path::Path;
//! use xtask::{
//!     core_mutation::{CoreMutationError, CoreMutationOutcome},
//!     release_check::CapturedReleaseSource,
//! };
//! fn synthetic(
//!     captured: &CapturedReleaseSource,
//!     directory: &Path,
//!     image: &str,
//! ) -> Result<CoreMutationOutcome, CoreMutationError> {
//!     captured.read_core_mutation_artifact(directory, image, || Ok::<(), std::io::Error>(()))
//! }
//! ```
//!
//! Completed outcomes keep their image binding private.
//!
//! ```compile_fail
//! use xtask::core_mutation::CoreMutationOutcome;
//! fn altered(mut outcome: CoreMutationOutcome, image: &str) -> CoreMutationOutcome {
//!     outcome.image = image.into();
//!     outcome
//! }
//! ```

#[cfg(all(test, unix))]
mod native {
    use crate::{
        core_assurance::{normalize_manifest, FieldMutation},
        core_mutation::{self, CapturedCoreMutation, CoreMutationError, CoreMutationOutcome},
        release_check::{CapturedReleaseSource, SourceInputs},
    };
    use std::{
        fs,
        io::{self, Write},
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    const IMAGE: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn require(valid: bool, reason: &str) -> io::Result<()> {
        if valid {
            Ok(())
        } else {
            Err(io::Error::other(reason.to_owned()))
        }
    }

    fn preconditions(project: &Path, captured: &CapturedReleaseSource) -> io::Result<()> {
        let mutation = CapturedCoreMutation::read(project).map_err(|error| {
            io::Error::other(format!(
                "Mutation fixture core capture precondition: {error}"
            ))
        })?;
        let inputs = SourceInputs::new(mutation.inputs()).map_err(io::Error::other)?;
        captured
            .inputs()
            .and_then(|release| release.bind(&inputs))
            .map_err(io::Error::other)?;
        let text = |path: &str| -> io::Result<&str> {
            let bytes = mutation
                .inputs()
                .find_map(|(actual, bytes)| (actual == Path::new(path)).then_some(bytes))
                .ok_or_else(|| io::Error::other("Mutation fixture input is missing"))?;
            std::str::from_utf8(bytes).map_err(io::Error::other)
        };
        normalize_manifest(
            text("crates/core/assurance/Cargo.toml")?,
            mutation.targets(),
        )
        .map_err(|error| {
            io::Error::other(format!(
                "Mutation fixture normalization precondition: {error}"
            ))
        })?;
        let original = text("crates/core/src/lib.rs")?;
        for field in FieldMutation::ALL {
            let changed = field.apply(original).map_err(|error| {
                io::Error::other(format!("Mutation fixture field precondition: {error}"))
            })?;
            require(
                changed != original,
                "Mutation fixture field had no source effect",
            )?;
        }
        core_mutation::plan(IMAGE).map_err(|error| {
            io::Error::other(format!("Mutation fixture image precondition: {error}"))
        })?;
        Ok(())
    }

    fn using(
        operation: impl FnOnce(&CapturedReleaseSource, &Path) -> io::Result<()>,
    ) -> io::Result<()> {
        let project = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or_else(|| io::Error::other("Mutation fixture project is missing"))?;
        let captured = CapturedReleaseSource::read(project).map_err(io::Error::other)?;
        preconditions(project, &captured)?;
        let parent = fs::canonicalize(std::env::temp_dir())?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let mut reserved = None;
        for attempt in 0..32 {
            let root = parent.join(format!(
                "rusterisk-mutation-artifact-tests-{}-{stamp}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&root) {
                Ok(()) => {
                    reserved = Some(root);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        let root = reserved.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Mutation fixture reservation failed",
            )
        })?;
        let primary = operation(&captured, &root);
        let cleanup = fs::remove_dir_all(root);
        match (primary, cleanup) {
            (primary, Ok(())) => primary,
            (Ok(()), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
                "{primary}; mutation artifact fixture cleanup failed: {cleanup}"
            ))),
        }
    }

    fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(bytes)?;
        file.flush()
    }

    fn io_rejection(
        result: Result<CoreMutationOutcome, CoreMutationError>,
        kind: io::ErrorKind,
    ) -> io::Result<()> {
        match result {
            Err(CoreMutationError::Io(error)) if error.kind() == kind => Ok(()),
            Err(error) => Err(io::Error::other(format!(
                "Expected mutation artifact IO rejection {kind:?}: {error}"
            ))),
            Ok(_) => Err(io::Error::other(
                "Invalid mutation artifact produced a completed outcome",
            )),
        }
    }

    #[test]
    fn mutation_artifact_production_rejects_nonimmutable_images_before_state_creation(
    ) -> io::Result<()> {
        using(|captured, root| {
            let destination = root.join("published");
            for image in ["", "mutable:tag", "sha256:abc"] {
                match captured.verify_core_mutation_artifact(image, &destination) {
                    Err(CoreMutationError::Plan) => {}
                    Err(error) => {
                        return Err(io::Error::other(format!(
                            "Invalid image reached another mutation phase: {error}"
                        )));
                    }
                    Ok(_) => {
                        return Err(io::Error::other(
                            "A nonimmutable image produced a completed mutation outcome",
                        ));
                    }
                }
                require(
                    !destination.try_exists()? && fs::read_dir(root)?.next().transpose()?.is_none(),
                    "Rejected mutation production created publication or staging state",
                )?;
            }
            Ok(())
        })
    }

    #[test]
    fn mutation_artifact_readers_require_a_published_directory() -> io::Result<()> {
        using(|captured, root| {
            io_rejection(
                captured.read_core_mutation_artifact(&root.join("missing"), IMAGE),
                io::ErrorKind::NotFound,
            )?;
            let file = root.join("diagnostic.txt");
            write(&file, b"incomplete native diagnostics\n")?;
            io_rejection(
                captured.read_core_mutation_artifact(&file, IMAGE),
                io::ErrorKind::InvalidData,
            )?;
            require(
                fs::read(file)? == b"incomplete native diagnostics\n",
                "Mutation artifact rejection changed its input file",
            )
        })
    }

    #[test]
    fn mutation_artifact_readers_cannot_import_unpublished_native_diagnostics() -> io::Result<()> {
        using(|captured, root| {
            let state = root.join("state");
            fs::create_dir(&state)?;
            for phase in [
                "mutation-inventory",
                "mutations",
                "terminal-state-build",
                "terminal-state-tests",
                "connection-claim-build",
                "connection-claim-tests",
            ] {
                write(
                    &state.join(format!("{phase}.txt")),
                    b"incomplete native output\n",
                )?;
            }
            let mutants = state.join("results/mutants.out");
            fs::create_dir_all(&mutants)?;
            write(&mutants.join("outcomes.json"), b"{\n")?;
            io_rejection(
                captured.read_core_mutation_artifact(root, IMAGE),
                io::ErrorKind::NotFound,
            )?;
            require(
                !root.join("native-check.toml").try_exists()?
                    && fs::read(mutants.join("outcomes.json"))? == b"{\n",
                "Mutation artifact reader published or changed an incomplete diagnostic tree",
            )
        })
    }

    #[test]
    fn mutation_artifact_readers_reject_linked_roots_without_changing_the_target() -> io::Result<()>
    {
        using(|captured, root| {
            let target = root.join("target");
            fs::create_dir(&target)?;
            let marker = target.join("marker.txt");
            write(&marker, b"preserved\n")?;
            let link = root.join("linked");
            std::os::unix::fs::symlink(&target, &link)?;
            io_rejection(
                captured.read_core_mutation_artifact(&link, IMAGE),
                io::ErrorKind::InvalidData,
            )?;
            require(
                fs::symlink_metadata(link)?.file_type().is_symlink()
                    && fs::read(marker)? == b"preserved\n",
                "Mutation artifact rejection changed the linked target",
            )
        })
    }
}
