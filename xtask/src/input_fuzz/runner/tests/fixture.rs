use super::{CommandMode, LinuxTarget, Phase, Scope, Target, ARCH};
use crate::process::ProcessError;
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const IMAGE: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    None,
    WrongRust,
    WrongArchitecture,
    WrongFuzzVersion,
    MissingInventory,
    IncompleteRun,
    ZeroRuns,
    Timeout,
    Crash,
    OversizedOutput,
    ChangedSource,
    AddedSourceConfiguration,
    ChangedHarness,
    AddedHarnessSource,
    AddedHarnessConfiguration,
    MissingHarness,
    Interrupted(Phase),
    ProcessFailure(Phase),
    PreserveFailure,
    Artifact,
    InvalidArtifact,
    OversizedArtifact,
    ExcessArtifacts,
    #[cfg(unix)]
    LinkedOutput,
    #[cfg(unix)]
    LinkedCache,
    #[cfg(unix)]
    BlockedCleanup,
}

pub fn count(target: Target) -> u64 {
    match target {
        Target::Settings => 1001,
        Target::Controls => 1002,
        Target::Sip => 1003,
        Target::Media => 1004,
    }
}
pub fn phases(scope: Scope) -> Vec<Phase> {
    let mut phases: Vec<_> = [
        Phase::RustVersion,
        Phase::Version,
        Phase::Inventory,
        Phase::Fetch,
    ]
    .into_iter()
    .collect();
    let targets: Vec<_> = match scope {
        Scope::All => Target::ALL.into(),
        Scope::Target(target) => [target].into(),
    };
    for target in targets {
        phases.extend([Phase::Build(target), Phase::Run(target)]);
    }
    phases
}

pub struct Fixture {
    pub root: PathBuf,
    pub fault: Fault,
    pub phases: Vec<Phase>,
    pub temporary: Option<PathBuf>,
    pub identity: Option<String>,
    pub cleanups: usize,
}
impl Fixture {
    pub fn working(&self) -> io::Result<&Path> {
        self.temporary
            .as_deref()
            .ok_or_else(|| io::Error::other("fuzz execution missing"))
    }
    fn report(&self, phase: Phase) -> Vec<u8> {
        match phase {
            Phase::RustVersion => {
                let commit = if self.fault == Fault::WrongRust {
                    "wrong"
                } else {
                    "8925ea358a0f265ca61026aadc7ecc506c545cbe"
                };
                let architecture = if self.fault == Fault::WrongArchitecture {
                    LinuxTarget::Amd64
                } else {
                    ARCH
                };
                format!("commit-hash: {commit}\nhost: {}\n", architecture.triple()).into_bytes()
            }
            Phase::Version => {
                if self.fault == Fault::WrongFuzzVersion {
                    b"cargo-fuzz 0.13.1\n".to_vec()
                } else {
                    b"cargo-fuzz 0.13.2\n".to_vec()
                }
            }
            Phase::Inventory => {
                let text = if self.fault == Fault::MissingInventory {
                    "settings\n".to_owned()
                } else {
                    format!("{}\n", Target::ALL.map(Target::name).join("\n"))
                };
                text.into_bytes()
            }
            Phase::Fetch | Phase::Build(_) => b"native build output\n".to_vec(),
            Phase::Run(target) => {
                let count = if self.fault == Fault::ZeroRuns {
                    0
                } else {
                    count(target)
                };
                let mut text = format!(
                    "#{count} DONE cov: 45 ft: 70 corp: 10/1Kb\nDone {count} runs in 1 second(s)\n"
                );
                if self.fault != Fault::IncompleteRun {
                    text.push_str(&format!("stat::number_of_executed_units: {count}\n"));
                }
                if self.fault == Fault::Crash {
                    text.push_str("==1==ERROR: AddressSanitizer: heap-buffer-overflow\n");
                } else if self.fault == Fault::Timeout {
                    text.push_str("libFuzzer: timeout after 2 seconds\n");
                }
                text.into_bytes()
            }
        }
    }
    pub fn perform(
        &mut self,
        command: &mut Command,
        budget: Duration,
        mode: CommandMode,
    ) -> Result<(), ProcessError> {
        let args: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        if mode == CommandMode::Cleanup {
            assert_eq!(args.len(), 3);
            assert_eq!(args[0], "rm");
            assert_eq!(args[1], "--force");
            let phase = self.phases.last().ok_or(ProcessError::InvalidOutput)?;
            assert!(args[2].ends_with(&phase.label()));
            assert_eq!(budget.as_secs(), 30);
            self.cleanups += 1;
            return Ok(());
        }
        let name = args
            .windows(2)
            .find(|pair| pair[0] == "--name")
            .map(|pair| pair[1].as_str())
            .ok_or(ProcessError::InvalidOutput)?;
        let phase = phases(Scope::All)
            .into_iter()
            .find(|phase| name.ends_with(&phase.label()))
            .ok_or(ProcessError::InvalidOutput)?;
        assert_eq!(
            budget.as_secs(),
            match phase {
                Phase::RustVersion | Phase::Version | Phase::Inventory => 30,
                Phase::Fetch | Phase::Build(_) => 900,
                Phase::Run(_) => 90,
            }
        );
        assert!(args.iter().any(|argument| argument == IMAGE));
        let state = mount(&args, ",target=/state")?;
        let source = mount(&args, ",target=/project,readonly")?;
        self.temporary = state.parent().map(Path::to_owned);
        self.identity = self
            .working()?
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("rusterisk-input-fuzz-"))
            .map(str::to_owned);
        self.phases.push(phase);
        let output = state.join(format!("{}.txt", phase.label()));
        fs::write(&output, self.report(phase))?;
        if phase == Phase::RustVersion {
            for target in Target::ALL {
                let corpus = state.join("corpus").join(target.name());
                if corpus.exists() {
                    for seed in target.seeds() {
                        assert_eq!(fs::read(corpus.join(seed.name))?, seed.bytes);
                    }
                }
            }
            if self.fault == Fault::OversizedOutput {
                fs::OpenOptions::new()
                    .write(true)
                    .open(&output)?
                    .set_len(4 * 1024 * 1024 + 1)?;
            }
            #[cfg(unix)]
            if self.fault == Fault::LinkedOutput {
                let outside = self.root.join("outside-native.txt");
                fs::write(&outside, self.report(phase))?;
                fs::remove_file(&output)?;
                std::os::unix::fs::symlink(outside, &output)?;
            }
        }
        if phase == Phase::Fetch {
            if self.fault == Fault::ChangedSource {
                fs::write(source.join("xtask/src/lib.rs"), b"changed")?;
            } else if self.fault == Fault::AddedSourceConfiguration {
                write(&source.join(".cargo/config.toml"), b"changed")?;
            } else if self.fault == Fault::ChangedHarness {
                fs::write(state.join("fuzz/src/lib.rs"), b"changed")?;
            } else if self.fault == Fault::AddedHarnessSource {
                write(&state.join("fuzz/src/hidden.rs"), b"changed")?;
            } else if self.fault == Fault::AddedHarnessConfiguration {
                write(&state.join("fuzz/.cargo/config.toml"), b"changed")?;
            } else if self.fault == Fault::MissingHarness {
                fs::remove_file(state.join("fuzz/fuzz_targets/settings.rs"))?;
            }
        }
        if phase == Phase::Run(Target::Settings) {
            if matches!(
                self.fault,
                Fault::Artifact
                    | Fault::Crash
                    | Fault::PreserveFailure
                    | Fault::InvalidArtifact
                    | Fault::OversizedArtifact
                    | Fault::ExcessArtifacts
            ) {
                let artifacts = state.join("artifacts/settings");
                let name = if self.fault == Fault::InvalidArtifact {
                    "bad.name"
                } else {
                    "crash-deadbeef"
                };
                let path = artifacts.join(name);
                fs::write(&path, b"failing input")?;
                if self.fault == Fault::OversizedArtifact {
                    fs::OpenOptions::new()
                        .write(true)
                        .open(&path)?
                        .set_len(65537)?;
                } else if self.fault == Fault::ExcessArtifacts {
                    for index in 0..16 {
                        fs::write(artifacts.join(format!("crash-{index}")), b"failing input")?;
                    }
                } else if self.fault == Fault::PreserveFailure {
                    fs::write(self.root.join(".cache"), b"blocked")?;
                    return Err(ProcessError::Interrupted);
                }
            }
            #[cfg(unix)]
            if self.fault == Fault::LinkedCache {
                let outside = self.root.join("outside-cache");
                fs::create_dir(&outside)?;
                std::os::unix::fs::symlink(outside, self.root.join(".cache"))?;
                fs::write(
                    state.join("artifacts/settings/crash-deadbeef"),
                    b"failing input",
                )?;
                return Err(ProcessError::Interrupted);
            }
        }
        #[cfg(unix)]
        if phase == Phase::Run(Target::Media) && self.fault == Fault::BlockedCleanup {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(state, fs::Permissions::from_mode(0o555))?;
        }
        if self.fault == Fault::Interrupted(phase) {
            return Err(ProcessError::Interrupted);
        }
        if self.fault == Fault::ProcessFailure(phase) {
            return Err(failed());
        }
        Ok(())
    }
    fn close(self) -> io::Result<()> {
        if let Some(path) = self.temporary {
            if path.exists() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let state = path.join("state");
                    if state.exists() {
                        fs::set_permissions(state, fs::Permissions::from_mode(0o755))?;
                    }
                }
                fs::remove_dir_all(path)?;
            }
        }
        fs::remove_dir_all(self.root)
    }
}
fn failed() -> ProcessError {
    #[cfg(unix)]
    let status = {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1 << 8)
    };
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1)
    };
    ProcessError::Status(status)
}
fn mount(args: &[String], suffix: &str) -> Result<PathBuf, ProcessError> {
    args.iter()
        .find_map(|argument| {
            argument
                .strip_prefix("type=bind,source=")
                .and_then(|path| path.strip_suffix(suffix))
        })
        .map(PathBuf::from)
        .ok_or(ProcessError::InvalidOutput)
}
fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| io::Error::other("fixture parent"))?,
    )?;
    fs::write(path, bytes)
}
pub fn using_fixture(test: impl FnOnce(&mut Fixture) -> io::Result<()>) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-input-fuzz-fixture-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root)?;
    let mut fixture = Fixture {
        root,
        fault: Fault::None,
        phases: Vec::new(),
        temporary: None,
        identity: None,
        cleanups: 0,
    };
    let operation = (|| {
        write(
            &fixture.root.join("Cargo.toml"),
            crate::input_fuzz::TEST_WORKSPACE.as_bytes(),
        )?;
        for path in ["Cargo.lock", "xtask/fuzz/Cargo.lock"] {
            write(
                &fixture.root.join(path),
                b"[[package]]\nname='fixture'\nversion='0.1.0'\n",
            )?;
        }
        write(
            &fixture.root.join("xtask/fuzz/Cargo.toml"),
            include_bytes!("../../../../fuzz/Cargo.toml"),
        )?;
        write(
            &fixture.root.join("xtask/fuzz/src/targets.rs"),
            include_bytes!("../../../../fuzz/src/targets.rs"),
        )?;
        for path in [
            "xtask/Cargo.toml",
            "xtask/src/lib.rs",
            "xtask/tests/contract.rs",
            "xtask/fuzz/src/lib.rs",
            "xtask/fuzz/tests/contract.rs",
        ] {
            write(&fixture.root.join(path), b"fixture")?;
        }
        for target in Target::ALL {
            write(
                &fixture
                    .root
                    .join("xtask/fuzz/fuzz_targets")
                    .join(format!("{}.rs", target.name())),
                b"fixture",
            )?;
        }
        test(&mut fixture)
    })();
    match (operation, fixture.close()) {
        (result, Ok(())) => result,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; fuzz fixture cleanup failed: {cleanup}"
        ))),
    }
}
