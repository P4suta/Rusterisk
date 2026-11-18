use super::{corpus_files, preserve, FuzzExecution, WorkingFuzz};
use crate::input_fuzz::{
    runner::tests::fixture::{count, using_fixture, Fixture, IMAGE},
    CapturedInputFuzz, FuzzError, LinuxTarget, Scope, Target,
};
use std::{fs, io, path::Path};

const ARCHITECTURE: LinuxTarget = LinuxTarget::Arm64;

fn using_retained(
    test: impl FnOnce(&Fixture, &Path, &FuzzExecution) -> io::Result<()>,
) -> io::Result<()> {
    using_fixture(|fixture| {
        let capture = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
        let work =
            WorkingFuzz::new(capture, Scope::All, IMAGE, ARCHITECTURE).map_err(io::Error::other)?;
        let operation = work.execute(
            &mut |command, budget, mode| fixture.perform(command, budget, mode),
            &mut Vec::new(),
        );
        let root = fixture.root.join("retained");
        let operation = operation.and_then(|runs| {
            preserve(&work, &root)?;
            Ok(runs)
        });
        let execution = FuzzExecution {
            identity: work.identity.clone(),
            directory: work.directory.clone(),
        };
        work.close(operation).map_err(io::Error::other)?;
        assert!(!fixture.working()?.exists());
        test(fixture, &root, &execution)
    })
}

fn accepted(fixture: &Fixture, root: &Path, execution: &FuzzExecution) -> io::Result<()> {
    let outcome = CapturedInputFuzz::read(&fixture.root)
        .and_then(|capture| capture.read_retained(IMAGE, ARCHITECTURE, root, execution))
        .map_err(io::Error::other)?;
    assert_eq!(outcome.architecture(), ARCHITECTURE);
    assert_eq!(outcome.image(), IMAGE);
    assert_eq!(outcome.runs().len(), Target::ALL.len());
    for (run, target) in outcome.runs().iter().zip(Target::ALL) {
        assert_eq!(run.target(), target);
        assert_eq!(run.executions(), count(target));
    }
    Ok(())
}

fn rejected(fixture: &Fixture, root: &Path, execution: &FuzzExecution) -> io::Result<()> {
    let result = CapturedInputFuzz::read(&fixture.root)
        .and_then(|capture| capture.read_retained(IMAGE, ARCHITECTURE, root, execution));
    assert!(result.is_err());
    Ok(())
}

#[test]
fn retained_fuzz_results_round_trip_after_owned_state_cleanup() -> io::Result<()> {
    using_retained(|fixture, root, execution| {
        accepted(fixture, root, execution)?;
        let result = CapturedInputFuzz::read(&fixture.root)
            .and_then(|capture| capture.read_retained(IMAGE, LinuxTarget::Amd64, root, execution));
        assert!(matches!(result, Err(FuzzError::Results)));
        Ok(())
    })
}

#[test]
fn retained_fuzz_results_reject_incomplete_reports_and_changed_seed_bytes() -> io::Result<()> {
    using_retained(|fixture, root, execution| {
        accepted(fixture, root, execution)?;
        for (path, bytes) in [
            (
                "state/rust-version.txt",
                b"commit-hash: changed\n".as_slice(),
            ),
            ("state/cargo-fuzz-version.txt", b"cargo-fuzz 0.13.1\n"),
            ("state/inventory.txt", b"settings\n"),
            ("state/run-settings.txt", b"incomplete native output\n"),
        ] {
            let path = root.join(path);
            let original = fs::read(&path)?;
            fs::write(&path, bytes)?;
            rejected(fixture, root, execution)?;
            fs::write(&path, original)?;
        }
        for target in Target::ALL {
            let path = root.join(format!("state/run-{}.txt", target.name()));
            let original = fs::read(&path)?;
            fs::remove_file(&path)?;
            rejected(fixture, root, execution)?;
            fs::write(&path, original)?;
        }
        let (path, _) = corpus_files()
            .next()
            .ok_or_else(|| io::Error::other("Seed is missing"))?;
        let path = root.join(path);
        let original = fs::read(&path)?;
        fs::write(&path, b"changed seed")?;
        rejected(fixture, root, execution)?;
        fs::write(path, original)?;
        accepted(fixture, root, execution)
    })
}

#[test]
fn retained_fuzz_results_reject_crash_inputs_and_changed_source() -> io::Result<()> {
    using_retained(|fixture, root, execution| {
        let crash = root.join("state/artifacts/settings/crash-input");
        fs::write(&crash, b"failing input")?;
        rejected(fixture, root, execution)?;
        fs::remove_file(crash)?;
        let source = root.join("source/xtask/fuzz/src/targets.rs");
        let original = fs::read(&source)?;
        fs::write(&source, b"changed target source")?;
        rejected(fixture, root, execution)?;
        fs::write(source, original)?;
        let unknown = root.join("state/unknown.txt");
        fs::write(&unknown, b"unknown native output")?;
        rejected(fixture, root, execution)?;
        fs::remove_file(unknown)?;
        accepted(fixture, root, execution)
    })
}
