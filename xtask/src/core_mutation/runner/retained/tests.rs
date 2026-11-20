use super::{verify_retained_with, MutationExecution, MutationPort};
use crate::{
    core_assurance::FieldMutation,
    core_mutation::{
        runner::tests::fixture::{
            using_fixture, Fault, Fixture, ASSURANCE_TEST_COUNT, IMAGE, PHASES,
        },
        CapturedCoreMutation, CoreMutationError, CoreMutationOutcome,
    },
    process::ProcessError,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Clone, Copy)]
enum Finish {
    Success,
    Failure,
}

struct Model<'a> {
    fixture: &'a mut Fixture,
    finish: Finish,
}

fn tool(arguments: &[String]) -> Option<(&'static str, &'static [u8])> {
    if arguments == ["--version"] {
        Some(("docker", b"Docker version 28.5.1, build abcdef1\n"))
    } else if arguments.last().is_some_and(|argument| argument == "-vV") {
        if arguments.iter().any(|argument| argument == "rustc") {
            Some(("rustc", b"rustc 1.95.0-nightly (8925ea358 2026-08-20)\nrelease: 1.95.0-nightly\ncommit-hash: 8925ea358a0f265ca61026aadc7ecc506c545cbe\nhost: aarch64-unknown-linux-gnu\n"))
        } else {
            Some(("cargo", b"cargo 1.95.0-nightly (012345678 2026-08-20)\nrelease: 1.95.0-nightly\ncommit-hash: 0123456789abcdef0123456789abcdef01234567\nhost: aarch64-unknown-linux-gnu\n"))
        }
    } else if arguments
        .last()
        .is_some_and(|argument| argument == "--version")
        && arguments.iter().any(|argument| argument == "mutants")
    {
        Some(("mutants", b"cargo-mutants 27.1.0\n"))
    } else {
        None
    }
}

impl MutationPort for Model<'_> {
    fn run(
        &mut self,
        command: &mut Command,
        budget: Duration,
        state: &Path,
    ) -> Result<(), ProcessError> {
        let arguments: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        if let Some((name, bytes)) = tool(&arguments) {
            assert_eq!(budget, Duration::from_secs(15));
            fs::write(state.join(format!("tools/{name}.stdout.txt")), bytes)?;
            Ok(())
        } else {
            self.fixture
                .perform(command, budget, super::super::CommandMode::Work)
        }
    }

    fn run_cleanup(
        &mut self,
        command: &mut Command,
        budget: Duration,
        _state: &Path,
    ) -> Result<(), ProcessError> {
        self.fixture
            .perform(command, budget, super::super::CommandMode::Cleanup)
    }

    fn finish(self) -> Result<(), ProcessError> {
        match self.finish {
            Finish::Success => Ok(()),
            Finish::Failure => Err(ProcessError::Interrupted),
        }
    }
}

fn capture(fixture: &mut Fixture) -> io::Result<CapturedCoreMutation> {
    let capture = CapturedCoreMutation::read(&fixture.root).map_err(io::Error::other)?;
    fixture.inventory(capture.targets().len(), capture.tests());
    Ok(capture)
}

fn produce(
    fixture: &mut Fixture,
    finish: Finish,
) -> io::Result<Result<(CoreMutationOutcome, MutationExecution), CoreMutationError>> {
    let capture = capture(fixture)?;
    let root = fixture.root.join("retained");
    Ok(verify_retained_with(
        capture,
        IMAGE,
        &root,
        Model { fixture, finish },
    ))
}

fn accepted(fixture: &Fixture, root: &Path, execution: &MutationExecution) -> io::Result<()> {
    let outcome = CapturedCoreMutation::read(&fixture.root)
        .and_then(|capture| capture.read_retained(IMAGE, root, execution))
        .map_err(io::Error::other)?;
    assert_eq!(outcome.image(), IMAGE);
    assert_eq!(outcome.test_count(), ASSURANCE_TEST_COUNT);
    Ok(())
}

fn rejected(fixture: &Fixture, root: &Path, execution: &MutationExecution) -> io::Result<()> {
    let result = CapturedCoreMutation::read(&fixture.root)
        .and_then(|capture| capture.read_retained(IMAGE, root, execution));
    assert!(result.is_err());
    Ok(())
}

#[test]
fn retained_mutation_round_trip_requires_actual_phase_exits_after_owned_cleanup() -> io::Result<()>
{
    using_fixture(|fixture| {
        let (outcome, mut execution) =
            produce(fixture, Finish::Success)?.map_err(io::Error::other)?;
        assert_eq!(outcome.image(), IMAGE);
        assert_eq!(outcome.test_count(), ASSURANCE_TEST_COUNT);
        assert!(fixture.phases == PHASES);
        assert!(!fixture.working()?.exists());
        assert_eq!(execution.exits, [0, 0, 0, 101, 0, 101]);
        let root = fixture.root.join("retained");
        accepted(fixture, &root, &execution)?;
        for index in 0..execution.exits.len() {
            let original = execution.exits[index];
            execution.exits[index] = if original == 101 { 0 } else { 101 };
            rejected(fixture, &root, &execution)?;
            execution.exits[index] = original;
        }
        accepted(fixture, &root, &execution)
    })
}

#[test]
fn retained_mutation_rejects_changed_tools_reports_and_every_source_projection() -> io::Result<()> {
    using_fixture(|fixture| {
        let (_, execution) = produce(fixture, Finish::Success)?.map_err(io::Error::other)?;
        let root = fixture.root.join("retained");
        accepted(fixture, &root, &execution)?;
        let paths = [
            "state/mutation-inventory.txt",
            "state/results/mutants.out/outcomes.json",
            "state/terminal-state-tests.txt",
            "state/connection-claim-tests.txt",
            "state/tools/docker.stdout.txt",
            "state/tools/rustc.stdout.txt",
            "state/tools/cargo.stdout.txt",
            "state/tools/mutants.stdout.txt",
            "original/crates/core/src/lib.rs",
            "source/src/lib.rs",
            "source/Cargo.toml",
        ]
        .map(PathBuf::from);
        for path in paths
            .into_iter()
            .chain(FieldMutation::ALL.into_iter().flat_map(|mutation| {
                [
                    Path::new(mutation.label()).join("src/lib.rs"),
                    Path::new(mutation.label()).join("Cargo.toml"),
                ]
            }))
        {
            let path = root.join(path);
            let original = fs::read(&path)?;
            fs::write(&path, b"changed retained input")?;
            rejected(fixture, &root, &execution)?;
            fs::write(&path, original)?;
        }
        let unknown = root.join("state/unknown.txt");
        fs::write(&unknown, b"unknown native report")?;
        rejected(fixture, &root, &execution)?;
        fs::remove_file(unknown)?;
        accepted(fixture, &root, &execution)
    })
}

#[test]
fn retained_mutation_refuses_outcomes_after_finish_or_owned_cleanup_failure() -> io::Result<()> {
    for (finish, blocked) in [
        (Finish::Success, true),
        (Finish::Failure, false),
        (Finish::Failure, true),
    ] {
        using_fixture(|fixture| {
            if blocked {
                fixture.fault = Fault::BlockedCleanup;
            }
            let error = produce(fixture, finish)?
                .err()
                .ok_or_else(|| io::Error::other("Failed mutation cleanup produced an outcome"))?;
            if !blocked {
                assert!(
                    matches!(error, CoreMutationError::Process(ProcessError::Interrupted)),
                    "{error}"
                );
                assert!(!fixture.working()?.exists());
            } else if matches!(finish, Finish::Success) {
                assert!(
                    matches!(error, CoreMutationError::Io(ref cause) if cause.kind() == io::ErrorKind::PermissionDenied),
                    "{error}"
                );
                assert!(fixture.working()?.exists());
            } else {
                let CoreMutationError::FollowUp {
                    operation,
                    context,
                    follow_up,
                } = &error
                else {
                    return Err(io::Error::other(format!(
                        "Combined mutation failure lost its causes: {error}"
                    )));
                };
                assert_eq!(operation, &ProcessError::Interrupted.to_string());
                assert_eq!(*context, "mutation temporary cleanup failed");
                assert!(!follow_up.is_empty());
                assert!(fixture.working()?.exists());
            }
            assert!(!fixture.root.join("retained/native-check.toml").exists());
            Ok(())
        })?;
    }
    Ok(())
}
