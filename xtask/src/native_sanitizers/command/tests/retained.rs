use super::{
    finish,
    fixture::{using_fixture, Fixture, IMAGE},
    CapturedNative,
};
use crate::native_sanitizers::{command::retained::NativeExecution, NativeOutcome};
use std::{fs, io};

fn using(
    test: impl FnOnce(&mut Fixture<'_>, NativeOutcome, NativeExecution) -> io::Result<()>,
) -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = CapturedNative::read(fixture.root).map_err(io::Error::other)?;
        let checked = captured.run_checked_with(
            &fixture.destination.clone(),
            IMAGE,
            |command, budget, mode| fixture.perform(command, budget, mode),
        );
        let outcome = finish(captured, checked, Ok(()), fixture.destination.clone())
            .map_err(io::Error::other)?;
        let execution = NativeExecution::capture(&outcome).map_err(io::Error::other)?;
        test(fixture, outcome, execution)
    })
}

fn changed(
    execution: &NativeExecution,
    field: &str,
    value: serde_json::Value,
) -> io::Result<NativeExecution> {
    let mut encoded = serde_json::to_value(execution).map_err(io::Error::other)?;
    encoded[field] = value;
    serde_json::from_value(encoded).map_err(io::Error::other)
}

fn rejected(fixture: &Fixture<'_>, execution: &NativeExecution) -> io::Result<()> {
    let captured = CapturedNative::read(fixture.root).map_err(io::Error::other)?;
    assert!(captured
        .read_retained(IMAGE, &fixture.destination, execution)
        .is_err());
    Ok(())
}

#[test]
fn retained_native_models_reconstruct_complete_inventory_and_artifact_paths() -> io::Result<()> {
    using(|fixture, outcome, execution| {
        assert_eq!(outcome.test_count(), 8);
        assert!(!fixture.working()?.exists());
        let captured = CapturedNative::read(fixture.root).map_err(io::Error::other)?;
        let restored = captured
            .read_retained(IMAGE, &fixture.destination, &execution)
            .map_err(io::Error::other)?;
        assert_eq!(restored.image(), IMAGE);
        assert_eq!(restored.test_count(), 8);
        assert_eq!(restored.artifacts(), fixture.destination);
        assert_eq!(
            restored.inputs().collect::<Vec<_>>(),
            outcome.inputs().collect::<Vec<_>>()
        );
        Ok(())
    })
}

#[test]
fn retained_native_models_use_recorded_linux_architecture_instead_of_reader_architecture(
) -> io::Result<()> {
    using(|fixture, _, execution| {
        let architecture = match std::env::consts::ARCH {
            "aarch64" => "x86_64",
            "x86_64" => "aarch64",
            _ => {
                return Err(io::Error::other(
                    "Unsupported native model host architecture",
                ))
            }
        };
        fs::write(
            fixture.destination.join("rust-version.txt"),
            format!("release: 1.99.0\nhost: {architecture}-unknown-linux-gnu\n"),
        )?;
        rejected(fixture, &execution)?;
        let recorded = changed(&execution, "architecture", architecture.into())?;
        let captured = CapturedNative::read(fixture.root).map_err(io::Error::other)?;
        let restored = captured
            .read_retained(IMAGE, &fixture.destination, &recorded)
            .map_err(io::Error::other)?;
        assert_eq!(restored.test_count(), 8);
        Ok(())
    })
}

#[test]
fn retained_native_models_reject_wrong_probe_codes_paths_and_incomplete_reports() -> io::Result<()>
{
    using(|fixture, _, execution| {
        for codes in [[0, 87], [86, 0], [86, 86]] {
            let invalid = changed(&execution, "probe_codes", serde_json::json!(codes))?;
            rejected(fixture, &invalid)?;
        }
        let invalid = changed(
            &execution,
            "directory",
            "/tmp/rusterisk-native-sanitizers-invalid/../foreign".into(),
        )?;
        rejected(fixture, &invalid)?;
        for (path, changed) in [
            ("inventory-media.txt", b"incomplete inventory\n".as_slice()),
            ("run-storage.txt", b"incomplete execution\n".as_slice()),
            ("inspect-opus.txt", b" U __asan_report_load8\n".as_slice()),
            ("SOURCE.sha256", b"incorrect source\n".as_slice()),
            (
                "source/crates/core/src/lib.rs",
                b"changed source\n".as_slice(),
            ),
        ] {
            let path = fixture.destination.join(path);
            let original = fs::read(&path)?;
            fs::write(&path, changed)?;
            rejected(fixture, &execution)?;
            fs::write(path, original)?;
        }
        let unknown = fixture.destination.join("unknown.txt");
        fs::write(&unknown, b"unknown output")?;
        rejected(fixture, &execution)?;
        fs::remove_file(unknown)?;
        let path = fixture.destination.join("run-tls.txt");
        let original = fs::read(&path)?;
        fs::remove_file(&path)?;
        std::os::unix::fs::symlink(fixture.destination.join("run-media.txt"), &path)?;
        rejected(fixture, &execution)?;
        fs::remove_file(&path)?;
        fs::write(path, original)?;
        Ok(())
    })
}
