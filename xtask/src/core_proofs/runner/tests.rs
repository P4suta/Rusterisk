use super::{finish, verify_with, CapturedCoreProofs, CommandMode};
use crate::{
    core_proofs::{ProofError, HARNESSES},
    process::ProcessError,
    release_check::SourceInputs,
};
use std::io;
mod artifacts;
pub(crate) mod fixture;
use fixture::{using_fixture, Fault, IMAGE};

#[test]
fn proof_plans_do_not_create_native_state_or_contact_the_daemon() -> io::Result<()> {
    using_fixture(|fixture| {
        let capture = CapturedCoreProofs::read(&fixture.root).map_err(io::Error::other)?;
        let plan = super::plan(IMAGE).map_err(io::Error::other)?;
        assert!(plan
            .run()
            .arguments
            .iter()
            .any(|argument| argument == IMAGE));
        assert!(fixture.temporary.is_none());
        assert_eq!(
            capture
                .inputs()
                .filter(|(path, _)| path.starts_with("crates/core"))
                .count(),
            capture.inputs().count()
        );
        Ok(())
    })
}

#[test]
fn proof_outcomes_bind_the_actual_source_image_and_all_harnesses_after_native_json_deletion(
) -> io::Result<()> {
    for fault in [Fault::None, Fault::FullResult] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let capture = CapturedCoreProofs::read(&fixture.root).map_err(io::Error::other)?;
            let expected = SourceInputs::new(capture.inputs())
                .map_err(io::Error::other)?
                .key();
            std::fs::write(
                fixture.root.join("crates/core/src/lib.rs"),
                b"concurrent live edit",
            )?;
            let checked = verify_with(capture, IMAGE, |command, budget, mode| {
                fixture.perform(command, budget, mode)
            });
            let outcome = finish(checked, Ok(())).map_err(io::Error::other)?;
            assert_eq!(outcome.image(), IMAGE);
            assert_eq!(outcome.harnesses(), HARNESSES);
            assert_eq!(
                SourceInputs::new(outcome.inputs())
                    .map_err(io::Error::other)?
                    .key(),
                expected
            );
            assert_eq!(fixture.runs, 1);
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn successful_commands_require_a_bounded_regular_complete_current_native_result() -> io::Result<()>
{
    for fault in [
        Fault::Missing,
        Fault::Invalid,
        Fault::WrongTool,
        Fault::MissingHarness,
        Fault::Uncovered,
        Fault::Oversized,
    ] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let capture = CapturedCoreProofs::read(&fixture.root).map_err(io::Error::other)?;
            let checked = verify_with(capture, IMAGE, |command, budget, mode| {
                fixture.perform(command, budget, mode)
            });
            assert!(finish(checked, Ok(())).is_err());
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn captured_proof_input_changes_prevent_completion_even_with_a_valid_native_report(
) -> io::Result<()> {
    using_fixture(|fixture| {
        fixture.fault = Fault::ChangedSource;
        let capture = CapturedCoreProofs::read(&fixture.root).map_err(io::Error::other)?;
        let checked = verify_with(capture, IMAGE, |command, budget, mode| {
            fixture.perform(command, budget, mode)
        });
        assert!(finish(checked, Ok(())).is_err());
        assert!(!fixture.working()?.exists());
        Ok(())
    })
}

#[test]
fn interrupted_proofs_keep_container_and_supervisor_cleanup_failures() -> io::Result<()> {
    for fault in [Fault::Interrupted, Fault::TimedOut] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let capture = CapturedCoreProofs::read(&fixture.root).map_err(io::Error::other)?;
            let checked = verify_with(capture, IMAGE, |command, budget, mode| match mode {
                CommandMode::Work => fixture.perform(command, budget, mode),
                CommandMode::Cleanup => {
                    fixture.cleanups += 1;
                    Err(ProcessError::InvalidOutput)
                }
            });
            let error = finish(checked, Err(ProcessError::Interrupted))
                .err()
                .ok_or_else(|| io::Error::other("interrupted proof completed"))?
                .to_string();
            assert!(error.contains("proof cleanup failed"), "{error}");
            assert!(
                error.contains(if fault == Fault::TimedOut {
                    "deadline exceeded"
                } else {
                    "interrupted"
                }),
                "{error}"
            );
            assert_eq!(fixture.cleanups, 1);
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn supervisor_failure_after_native_success_still_prevents_an_outcome() -> io::Result<()> {
    using_fixture(|fixture| {
        let capture = CapturedCoreProofs::read(&fixture.root).map_err(io::Error::other)?;
        let checked = verify_with(capture, IMAGE, |command, budget, mode| {
            fixture.perform(command, budget, mode)
        });
        assert!(matches!(
            finish(checked, Err(ProcessError::InvalidOutput)),
            Err(ProofError::Process(ProcessError::InvalidOutput))
        ));
        assert!(!fixture.working()?.exists());
        Ok(())
    })
}

#[test]
fn proof_capture_preserves_exact_rust_file_and_individual_size_limits() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = CapturedCoreProofs::read(&fixture.root).map_err(io::Error::other)?;
        let initial = captured
            .inputs()
            .filter(|(path, _)| path.extension().is_some_and(|extension| extension == "rs"))
            .count();
        for index in initial..64 {
            std::fs::write(
                fixture
                    .root
                    .join(format!("crates/core/src/padding_{index}.rs")),
                b" ",
            )?;
        }
        assert_eq!(
            CapturedCoreProofs::read(&fixture.root)
                .map_err(io::Error::other)?
                .inputs()
                .count(),
            66
        );
        std::fs::write(fixture.root.join("crates/core/src/excess.rs"), b" ")?;
        assert!(CapturedCoreProofs::read(&fixture.root).is_err());
        Ok(())
    })?;
    using_fixture(|fixture| {
        let path = fixture.root.join("crates/core/src/padding.rs");
        let bytes = vec![b' '; 1024 * 1024];
        std::fs::write(&path, &bytes)?;
        CapturedCoreProofs::read(&fixture.root).map_err(io::Error::other)?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)?
            .set_len(1024 * 1024 + 1)?;
        assert!(CapturedCoreProofs::read(&fixture.root).is_err());
        Ok(())
    })?;
    using_fixture(|fixture| {
        let path = fixture.root.join("crates/core/proofs/Cargo.lock");
        let mut bytes = std::fs::read(&path)?;
        bytes.resize(16_384, b' ');
        std::fs::write(&path, bytes)?;
        CapturedCoreProofs::read(&fixture.root).map_err(io::Error::other)?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)?
            .set_len(16_385)?;
        assert!(CapturedCoreProofs::read(&fixture.root).is_err());
        Ok(())
    })
}

#[test]
fn proof_capture_preserves_exact_total_size_and_depth_limits() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = CapturedCoreProofs::read(&fixture.root).map_err(io::Error::other)?;
        let total: usize = captured.inputs().map(|(_, bytes)| bytes.len()).sum();
        let mut remaining = 8 * 1024 * 1024 - total;
        let mut index = 0;
        while remaining > 0 {
            let size = remaining.min(1024 * 1024);
            std::fs::write(
                fixture
                    .root
                    .join(format!("crates/core/src/padding_{index}.rs")),
                vec![b' '; size],
            )?;
            remaining -= size;
            index += 1;
        }
        let captured = CapturedCoreProofs::read(&fixture.root).map_err(io::Error::other)?;
        assert_eq!(
            captured
                .inputs()
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>(),
            8 * 1024 * 1024
        );
        std::fs::write(fixture.root.join("crates/core/src/excess.rs"), b" ")?;
        assert!(CapturedCoreProofs::read(&fixture.root).is_err());
        Ok(())
    })?;
    using_fixture(|fixture| {
        let mut directory = fixture.root.join("crates/core/src");
        for _ in 0..8 {
            directory.push("nested");
            std::fs::create_dir(&directory)?;
        }
        std::fs::write(directory.join("fixture.rs"), b" ")?;
        CapturedCoreProofs::read(&fixture.root).map_err(io::Error::other)?;
        directory.push("excess");
        std::fs::create_dir(&directory)?;
        assert!(CapturedCoreProofs::read(&fixture.root).is_err());
        Ok(())
    })
}

#[test]
fn proof_working_directory_reservations_preserve_existing_entries_and_bound_attempts(
) -> io::Result<()> {
    using_fixture(|fixture| {
        let parent = fixture.root.join("reservations");
        std::fs::create_dir(&parent)?;
        let foreign = parent.join("rusterisk-core-proofs-fixture");
        std::fs::create_dir(&foreign)?;
        std::fs::write(foreign.join("foreign"), b"keep")?;
        let (identity, directory) = super::reserve(&parent, "fixture").map_err(io::Error::other)?;
        assert_eq!(identity, "fixture-1");
        assert_ne!(directory, foreign);
        for index in 2..32 {
            std::fs::create_dir(parent.join(format!("rusterisk-core-proofs-fixture-{index}")))?;
        }
        assert!(
            matches!(super::reserve(&parent,"fixture"),Err(ProofError::Io(error)) if error.kind()==io::ErrorKind::AlreadyExists)
        );
        assert_eq!(std::fs::read(foreign.join("foreign"))?, b"keep");
        assert_eq!(
            std::fs::read_dir(parent)?
                .collect::<io::Result<Vec<_>>>()?
                .len(),
            32
        );
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn symlinked_native_output_and_failed_directory_cleanup_prevent_completion() -> io::Result<()> {
    for fault in [Fault::LinkedResult, Fault::BlockedCleanup] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let capture = CapturedCoreProofs::read(&fixture.root).map_err(io::Error::other)?;
            let checked = verify_with(capture, IMAGE, |command, budget, mode| {
                fixture.perform(command, budget, mode)
            });
            assert!(finish(checked, Ok(())).is_err());
            assert_eq!(fixture.working()?.exists(), fault == Fault::BlockedCleanup);
            Ok(())
        })?;
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn proof_capture_rejects_redirected_source_roots() -> io::Result<()> {
    using_fixture(|fixture| {
        let source = fixture.root.join("crates/core/src");
        let moved = fixture.root.join("outside");
        std::fs::rename(&source, &moved)?;
        std::os::unix::fs::symlink(&moved, &source)?;
        assert!(CapturedCoreProofs::read(&fixture.root).is_err());
        Ok(())
    })
}
