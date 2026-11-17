use super::{verify_with, NativeGate, NativeOutcome, NativePlan, NativePort};
use crate::{
    process::ProcessError,
    release_check::{CapturedReleaseSource, SourceInputs},
};
use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy)]
enum Fault {
    None,
    MissingInput,
    WorkFailure,
    TimedOut,
    Interrupted,
    CleanupFailure,
    WorkAndCleanupFailure,
    ChangedDuringRun,
    ChangedDuringCleanup,
}

struct Fixture {
    working: PathBuf,
    expected_entry: &'static str,
    fault: Fault,
    runs: usize,
    cleanups: usize,
    late_mutations: usize,
}

struct Port<'a> {
    fixture: &'a mut Fixture,
    captured: &'a CapturedReleaseSource,
}

fn require(valid: bool, reason: &'static str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(reason))
    }
}

impl NativePort for Port<'_> {
    fn run(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError> {
        require(!budget.is_zero(), "Native gate has no command budget")?;
        let source = command.get_current_dir().ok_or_else(|| {
            io::Error::other("Native gate did not select its captured working directory")
        })?;
        require(
            source.canonicalize()? == self.fixture.working.join("source").canonicalize()?,
            "Native gate ran outside its owned source copy",
        )?;
        let source = source.canonicalize()?;
        let environment = |name: &str| {
            command
                .get_envs()
                .find_map(|(key, value)| (key == name).then_some(value))
        };
        require(
            environment("MISE_TRUSTED_CONFIG_PATHS") == Some(Some(source.as_os_str()))
                && environment("MISE_CEILING_PATHS") == Some(Some(source.as_os_str())),
            "Native gate did not scope mise trust and config discovery to captured source",
        )?;
        require(
            environment("MISE_CONFIG_FILE") == Some(None) && environment("MISE_ENV") == Some(None),
            "Native gate inherited an external mise config or environment selector",
        )?;
        let isolated_config = self.fixture.working.join("state/mise-global.toml");
        require(
            environment("MISE_GLOBAL_CONFIG_FILE") == Some(Some(isolated_config.as_os_str()))
                && environment("MISE_SYSTEM_CONFIG_FILE")
                    == Some(Some(isolated_config.as_os_str()))
                && fs::read(&isolated_config)?.is_empty(),
            "Native gate retained global or system mise configuration outside owned state",
        )?;
        self.captured.check(&source).map_err(io::Error::other)?;
        require(
            command.get_program() == OsStr::new("mise")
                && command
                    .get_args()
                    .eq(["x", "--", "just", self.fixture.expected_entry]
                        .into_iter()
                        .map(OsStr::new)),
            "Native gate bypassed the maintained just entry",
        )?;
        self.fixture.runs = self
            .fixture
            .runs
            .checked_add(1)
            .ok_or(ProcessError::InvalidOutput)?;
        require(self.fixture.runs == 1, "Native gate repeated its entry")?;
        match self.fixture.fault {
            Fault::MissingInput => fs::remove_file(source.join("Cargo.lock"))?,
            Fault::ChangedDuringRun => {
                fs::write(source.join("Cargo.lock"), b"changed during native gate")?;
            }
            Fault::WorkFailure => return Err(ProcessError::InvalidOutput),
            Fault::TimedOut | Fault::WorkAndCleanupFailure => return Err(ProcessError::Timeout),
            Fault::Interrupted => return Err(ProcessError::Interrupted),
            Fault::None | Fault::CleanupFailure | Fault::ChangedDuringCleanup => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<(), ProcessError> {
        self.fixture.cleanups += 1;
        match self.fixture.fault {
            Fault::CleanupFailure | Fault::WorkAndCleanupFailure => {
                Err(ProcessError::InvalidOutput)
            }
            Fault::ChangedDuringCleanup => {
                fs::write(
                    self.fixture.working.join("source/Cargo.lock"),
                    b"changed while supervised children were reaped",
                )?;
                self.fixture.late_mutations += 1;
                Ok(())
            }
            Fault::None
            | Fault::MissingInput
            | Fault::WorkFailure
            | Fault::TimedOut
            | Fault::Interrupted
            | Fault::ChangedDuringRun => Ok(()),
        }
    }
}

fn using_fixture(
    entry: &'static str,
    test: impl FnOnce(&CapturedReleaseSource, &mut Fixture) -> io::Result<()>,
) -> io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("workspace missing"))?;
    let captured = CapturedReleaseSource::read(root).map_err(io::Error::other)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let parent = std::env::temp_dir().join(format!(
        "rusterisk-native-gate-tests-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&parent)?;
    let mut fixture = Fixture {
        working: parent.join("execution"),
        expected_entry: entry,
        fault: Fault::None,
        runs: 0,
        cleanups: 0,
        late_mutations: 0,
    };
    let operation = test(&captured, &mut fixture);
    let cleanup = fs::remove_dir_all(parent);
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; native gate fixture cleanup failed: {cleanup}"
        ))),
    }
}

#[test]
fn native_gate_outcomes_bind_the_executed_source_and_reuse_each_maintained_entry() -> io::Result<()>
{
    for (gate, entry) in [
        (NativeGate::Fast, "check"),
        (NativeGate::Security, "security"),
    ] {
        using_fixture(entry, |captured, fixture| {
            let expected = captured.inputs().map_err(io::Error::other)?.key();
            let working = fixture.working.clone();
            let plan = NativePlan::new(gate);
            let outcome = verify_with(captured, plan, &working, Port { fixture, captured })
                .map_err(io::Error::other)?;
            let key = match outcome {
                NativeOutcome::Fast(outcome) => {
                    require(entry == "check", "Security run returned a fast outcome")?;
                    SourceInputs::new(outcome.inputs())
                        .map_err(io::Error::other)?
                        .key()
                }
                NativeOutcome::Security(outcome) => {
                    require(entry == "security", "Fast run returned a security outcome")?;
                    SourceInputs::new(outcome.inputs())
                        .map_err(io::Error::other)?
                        .key()
                }
            };
            require(
                key == expected,
                "Native outcome changed its captured inputs",
            )?;
            require(
                fixture.runs == 1,
                "Native gate skipped or repeated its entry",
            )?;
            require(
                fixture.cleanups == 1,
                "Native supervisor was not finished exactly once",
            )?;
            require(!working.exists(), "Native gate retained its build state")
        })?;
    }
    Ok(())
}

#[test]
fn missing_failed_interrupted_or_changed_native_runs_never_produce_an_outcome() -> io::Result<()> {
    for fault in [
        Fault::MissingInput,
        Fault::WorkFailure,
        Fault::TimedOut,
        Fault::Interrupted,
        Fault::CleanupFailure,
        Fault::ChangedDuringRun,
        Fault::ChangedDuringCleanup,
    ] {
        using_fixture("check", |captured, fixture| {
            fixture.fault = fault;
            let working = fixture.working.clone();
            let plan = NativePlan::new(NativeGate::Fast);
            let result = verify_with(captured, plan, &working, Port { fixture, captured });
            require(
                result.is_err(),
                "Incomplete native execution produced an outcome",
            )?;
            require(
                fixture.cleanups == 1,
                "Failed native run skipped supervisor cleanup",
            )?;
            if matches!(fault, Fault::ChangedDuringCleanup) {
                require(
                    fixture.late_mutations == 1,
                    "Source was discarded before children were reaped",
                )?;
            }
            require(
                !working.exists(),
                "Failed native execution retained owned state",
            )
        })?;
    }
    Ok(())
}

#[test]
fn native_failure_and_supervisor_cleanup_failure_both_remain_identifiable() -> io::Result<()> {
    using_fixture("security", |captured, fixture| {
        fixture.fault = Fault::WorkAndCleanupFailure;
        let working = fixture.working.clone();
        let plan = NativePlan::new(NativeGate::Security);
        let error = verify_with(captured, plan, &working, Port { fixture, captured })
            .err()
            .ok_or_else(|| io::Error::other("Failed native gate produced an outcome"))?
            .to_string();
        require(
            error.contains("deadline exceeded"),
            "Native failure was discarded",
        )?;
        require(
            error.contains("invalid bounded process output"),
            "Supervisor cleanup failure was discarded",
        )?;
        require(
            fixture.cleanups == 1,
            "Combined failure skipped supervisor cleanup",
        )?;
        require(!working.exists(), "Combined failure retained owned state")
    })
}

fn captured_input(
    captured: &CapturedReleaseSource,
    path: &str,
) -> Result<Vec<u8>, crate::distribution::SdkInputError> {
    captured
        .files()
        .find_map(|(actual, bytes)| (actual == Path::new(path)).then(|| bytes.to_vec()))
        .ok_or_else(|| crate::distribution::SdkInputError::Input {
            path: path.to_owned(),
            source: io::Error::other("Native gate input was not captured"),
        })
}

#[test]
fn captured_native_inputs_supply_the_existing_distribution_and_security_readers() -> io::Result<()>
{
    using_fixture("check", |captured, fixture| {
        crate::distribution::check_sdk_inputs_with(|path| captured_input(captured, path))
            .map_err(io::Error::other)?;
        crate::distribution::check_dependency_paths_with(|path| captured_input(captured, path))
            .map_err(io::Error::other)?;
        for path in [
            "mise.toml",
            ".cargo/config.toml",
            "justfile",
            "rust-toolchain.toml",
            "clippy.toml",
            "deny.toml",
            "api/gitleaks.toml",
            "api/policy-exceptions.toml",
            "Cargo.lock",
            "xtask/fuzz/Cargo.lock",
            "xtask/src/main.rs",
            "xtask/src/security.rs",
            "tests/interop/node-ari-client/package.json",
            "tests/interop/node-ari-client/package-lock.json",
            "examples/browser/package.json",
            "examples/browser/package-lock.json",
            "vendor/str0m-source.toml",
            "vendor/str0m.patch",
            "vendor/archives/str0m-0.25.0.crate",
        ] {
            require(
                !captured_input(captured, path)
                    .map_err(io::Error::other)?
                    .is_empty(),
                "A maintained native gate reader has no captured input",
            )?;
        }
        fs::create_dir(&fixture.working)?;
        let source = fixture.working.join("source");
        captured
            .write(&source, &fixture.working.join("state"))
            .map_err(io::Error::other)?;
        crate::distribution::check_sdk_archive_inputs(&source).map_err(io::Error::other)?;
        captured.check(&source).map_err(io::Error::other)?;
        fs::remove_dir_all(&fixture.working)?;
        Ok(())
    })
}
