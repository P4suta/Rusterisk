use super::{CapturedAssurance, CommandMode, FieldMutation, Phase};
use crate::process::ProcessError;
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const IMAGE: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
pub const PHASES: [Phase; 6] = [
    Phase::MutationInventory,
    Phase::Mutations,
    Phase::FieldBuild(FieldMutation::TerminalState),
    Phase::FieldTests(FieldMutation::TerminalState),
    Phase::FieldBuild(FieldMutation::ConnectionClaim),
    Phase::FieldTests(FieldMutation::ConnectionClaim),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    None,
    MissingInventory,
    MissingCampaign,
    ChangedInventory,
    TruncatedCampaign,
    Missed,
    Timeout,
    WrongVersion,
    OversizedCampaign,
    FailedBuild(FieldMutation),
    Survived(FieldMutation),
    WrongExit(FieldMutation),
    MissingTests(FieldMutation),
    SkippedTests(FieldMutation),
    TruncatedTests(FieldMutation),
    ChangedOriginal,
    ChangedWorking,
    ChangedManifest,
    ChangedField(FieldMutation),
    ChangedPriorField,
    Interrupted(Phase),
    #[cfg(unix)]
    LinkedCampaign,
    #[cfg(unix)]
    LinkedResultsDirectory,
    #[cfg(unix)]
    BlockedCleanup,
}

#[derive(Clone, Serialize)]
struct Function {
    function_name: &'static str,
}
#[derive(Clone, Serialize)]
struct Mutant {
    name: String,
    file: &'static str,
    function: Function,
    replacement: &'static str,
}
fn mutants() -> Vec<Mutant> {
    [
        ("src/lib.rs", "Call::apply"),
        ("src/lib.rs", "Call::claim_connection"),
        ("src/stasis.rs", "Call::enter_stasis"),
        ("src/lib.rs", "Call::join"),
        ("src/media_tasks.rs", "MediaTasks::cancel"),
        (
            "src/resource_subscriptions.rs",
            "ResourceSubscriptions<N>::subscribe",
        ),
        ("src/transfer.rs", "Transfer::begin"),
        ("src/attended_transfer.rs", "AttendedTransfer::begin"),
        ("src/owned_call_pair.rs", "OwnedCallPair::new"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (file, function_name))| Mutant {
        name: format!("{file}:{}:1: replace {function_name}", index + 1),
        file,
        function: Function { function_name },
        replacement: "false",
    })
    .collect()
}
pub fn campaign(fault: Fault) -> io::Result<Vec<u8>> {
    let mutants = mutants();
    let baseline: Vec<_> = [
        serde_json::json!({"phase":"Build", "process_status":"Success"}),
        serde_json::json!({"phase":"Test", "process_status":"Success"}),
    ]
    .into_iter()
    .collect();
    let mut outcomes: Vec<serde_json::Value> = [serde_json::json!({
        "scenario":"Baseline", "summary":"Success", "phase_results":baseline
    })]
    .into_iter()
    .collect();
    for (index, mutant) in mutants.iter().enumerate() {
        if index < 2 {
            let phases: Vec<_> = [serde_json::json!({
                "phase":"Build", "process_status":{"Failure":101}
            })]
            .into_iter()
            .collect();
            outcomes.push(serde_json::json!({
                "scenario":{"Mutant":mutant}, "summary":"Unviable", "phase_results":phases
            }));
        } else {
            let phases: Vec<_> = [
                serde_json::json!({"phase":"Build", "process_status":"Success"}),
                serde_json::json!({"phase":"Test", "process_status":{"Failure":101}}),
            ]
            .into_iter()
            .collect();
            outcomes.push(serde_json::json!({
                "scenario":{"Mutant":mutant}, "summary":"CaughtMutant", "phase_results":phases
            }));
        }
    }
    let mut report = serde_json::json!({
        "cargo_mutants_version":"27.1.0", "end_time":"2026-11-17T00:00:00Z",
        "total_mutants":mutants.len(), "caught":mutants.len()-2, "unviable":2,
        "missed":0, "timeout":0, "success":0, "outcomes":outcomes
    });
    match fault {
        Fault::ChangedInventory => {
            report["outcomes"][1]["scenario"]["Mutant"]["name"] =
                serde_json::json!("unrelated mutation");
        }
        Fault::Missed => report["missed"] = serde_json::json!(1),
        Fault::Timeout => report["timeout"] = serde_json::json!(1),
        Fault::WrongVersion => report["cargo_mutants_version"] = serde_json::json!("27.0.0"),
        Fault::None
        | Fault::MissingInventory
        | Fault::MissingCampaign
        | Fault::TruncatedCampaign
        | Fault::OversizedCampaign
        | Fault::FailedBuild(_)
        | Fault::Survived(_)
        | Fault::WrongExit(_)
        | Fault::MissingTests(_)
        | Fault::SkippedTests(_)
        | Fault::TruncatedTests(_)
        | Fault::ChangedOriginal
        | Fault::ChangedWorking
        | Fault::ChangedManifest
        | Fault::ChangedField(_)
        | Fault::ChangedPriorField
        | Fault::Interrupted(_) => {}
        #[cfg(unix)]
        Fault::LinkedCampaign | Fault::LinkedResultsDirectory | Fault::BlockedCleanup => {}
    }
    let mut bytes = serde_json::to_vec(&report).map_err(io::Error::other)?;
    if fault == Fault::TruncatedCampaign {
        bytes.truncate(bytes.len() / 2);
    }
    Ok(bytes)
}

pub struct Fixture {
    pub root: PathBuf,
    pub fault: Fault,
    pub phases: Vec<Phase>,
    pub temporary: Option<PathBuf>,
    pub cleanups: usize,
    suites: Vec<Vec<String>>,
}
impl Fixture {
    pub fn working(&self) -> io::Result<&Path> {
        self.temporary
            .as_deref()
            .ok_or_else(|| io::Error::other("mutation execution missing"))
    }
    pub fn inventory(&mut self, count: usize, names: &BTreeSet<String>) {
        self.suites = (0..count).map(|_| Vec::new()).collect();
        for (index, name) in names.iter().enumerate() {
            self.suites[index % count].push(name.to_owned());
        }
    }
    fn field_report(&self, mutation: FieldMutation) -> io::Result<Vec<u8>> {
        let mut text = String::new();
        for (suite_index, suite) in self.suites.iter().enumerate() {
            let suite: Vec<_> = suite
                .iter()
                .enumerate()
                .filter(|(index, _)| {
                    self.fault != Fault::MissingTests(mutation) || suite_index != 0 || *index != 0
                })
                .map(|(_, name)| name)
                .collect();
            text.push_str(
                &serde_json::to_string(&serde_json::json!({
                    "type":"suite", "event":"started", "test_count":suite.len()
                }))
                .map_err(io::Error::other)?,
            );
            text.push('\n');
            let mut failed = 0;
            for (index, name) in suite.iter().enumerate() {
                let event = if suite_index == 0 && index == 0 {
                    failed += 1;
                    if self.fault == Fault::SkippedTests(mutation) {
                        "ignored"
                    } else {
                        "failed"
                    }
                } else {
                    "ok"
                };
                for event in ["started", event] {
                    text.push_str(
                        &serde_json::to_string(&serde_json::json!({
                            "type":"test", "event":event, "name":name
                        }))
                        .map_err(io::Error::other)?,
                    );
                    text.push('\n');
                }
            }
            text.push_str(
                &serde_json::to_string(&serde_json::json!({
                    "type":"suite", "event":if failed == 0 { "ok" } else { "failed" },
                    "passed":suite.len()-failed, "failed":failed,
                    "ignored":0, "measured":0, "filtered_out":0
                }))
                .map_err(io::Error::other)?,
            );
            text.push('\n');
        }
        let mut bytes = text.into_bytes();
        if self.fault == Fault::TruncatedTests(mutation) {
            bytes.truncate(bytes.len() / 2);
        }
        Ok(bytes)
    }
    pub fn perform(
        &mut self,
        command: &mut Command,
        budget: Duration,
        mode: CommandMode,
    ) -> Result<(), ProcessError> {
        assert_eq!(mode, CommandMode::Work);
        let args: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        let name = args
            .windows(2)
            .find(|pair| pair[0] == "--name")
            .map(|pair| pair[1].as_str())
            .ok_or(ProcessError::InvalidOutput)?;
        let phase = PHASES
            .into_iter()
            .find(|phase| name.ends_with(phase.label()))
            .ok_or(ProcessError::InvalidOutput)?;
        assert_eq!(
            budget.as_secs(),
            if matches!(phase, Phase::FieldBuild(_) | Phase::FieldTests(_)) {
                60
            } else {
                900
            }
        );
        assert!(args.iter().any(|argument| argument == IMAGE));
        self.phases.push(phase);
        let state = mount(&args, ",target=/state")?;
        let source = mount(&args, ",target=/project,readonly")?;
        self.temporary = state.parent().map(Path::to_owned);
        let output = state.join(format!("{}.txt", phase.label()));
        match phase {
            Phase::MutationInventory => {
                let bytes = if self.fault == Fault::MissingInventory {
                    b"[]".to_vec()
                } else {
                    serde_json::to_vec(&mutants()).map_err(io::Error::other)?
                };
                fs::write(output, bytes)?;
            }
            Phase::Mutations => {
                fs::write(output, b"native cargo-mutants stdout\n")?;
                let path = state.join("results/mutants.out/outcomes.json");
                fs::create_dir_all(path.parent().ok_or(ProcessError::InvalidOutput)?)?;
                if self.fault != Fault::MissingCampaign {
                    fs::write(&path, campaign(self.fault)?)?;
                }
                if self.fault == Fault::OversizedCampaign {
                    fs::OpenOptions::new()
                        .write(true)
                        .open(&path)?
                        .set_len(4 * 1024 * 1024 + 1)?;
                }
                #[cfg(unix)]
                if matches!(
                    self.fault,
                    Fault::LinkedCampaign | Fault::LinkedResultsDirectory
                ) {
                    let outside = self.root.join("outside");
                    fs::create_dir(&outside)?;
                    fs::write(outside.join("outcomes.json"), campaign(Fault::None)?)?;
                    if self.fault == Fault::LinkedCampaign {
                        fs::remove_file(&path)?;
                        std::os::unix::fs::symlink(outside.join("outcomes.json"), &path)?;
                    } else {
                        let parent = path.parent().ok_or(ProcessError::InvalidOutput)?;
                        fs::remove_dir_all(parent)?;
                        std::os::unix::fs::symlink(outside, parent)?;
                    }
                }
                if self.fault == Fault::ChangedOriginal {
                    fs::write(
                        self.working()?.join("original/crates/core/src/lib.rs"),
                        b"changed",
                    )?;
                } else if self.fault == Fault::ChangedWorking {
                    fs::write(source.join("src/lib.rs"), b"changed")?;
                } else if self.fault == Fault::ChangedManifest {
                    fs::write(source.join("Cargo.toml"), b"changed")?;
                }
            }
            Phase::FieldBuild(mutation) | Phase::FieldTests(mutation) => {
                if matches!(phase, Phase::FieldBuild(_)) {
                    let original = fs::read_to_string(
                        self.working()?.join("original/crates/core/src/lib.rs"),
                    )?;
                    assert_eq!(
                        fs::read(source.join("src/lib.rs"))?,
                        mutation
                            .apply(&original)
                            .map_err(io::Error::other)?
                            .as_bytes()
                    );
                    fs::write(output, b"native field build stdout\n")?;
                    if self.fault == Fault::FailedBuild(mutation) {
                        return failed(101);
                    }
                } else {
                    fs::write(output, self.field_report(mutation)?)?;
                    if self.fault == Fault::ChangedField(mutation) {
                        fs::write(source.join("src/lib.rs"), b"changed")?;
                    }
                    if self.fault == Fault::ChangedPriorField
                        && mutation == FieldMutation::ConnectionClaim
                    {
                        fs::write(
                            self.working()?.join("terminal-state/src/lib.rs"),
                            b"changed",
                        )?;
                    }
                    #[cfg(unix)]
                    if self.fault == Fault::BlockedCleanup
                        && mutation == FieldMutation::ConnectionClaim
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&state, fs::Permissions::from_mode(0o555))?;
                    }
                }
            }
            Phase::MiriSetup | Phase::MiriInventory | Phase::MiriTests => {
                return Err(ProcessError::InvalidOutput);
            }
        }
        if self.fault == Fault::Interrupted(phase) {
            return Err(ProcessError::Interrupted);
        }
        if let Phase::FieldTests(mutation) = phase {
            if self.fault == Fault::WrongExit(mutation) {
                failed(1)
            } else if self.fault == Fault::Survived(mutation) {
                Ok(())
            } else {
                failed(101)
            }
        } else {
            Ok(())
        }
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
fn failed(code: u8) -> Result<(), ProcessError> {
    #[cfg(unix)]
    let status = {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(i32::from(code) << 8)
    };
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(u32::from(code))
    };
    Err(ProcessError::Status(status))
}
pub fn using_fixture(test: impl FnOnce(&mut Fixture) -> io::Result<()>) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-mutation-fixture-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root)?;
    let mut fixture = Fixture {
        root,
        fault: Fault::None,
        phases: Vec::new(),
        temporary: None,
        cleanups: 0,
        suites: Vec::new(),
    };
    let operation = (|| {
        let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/core");
        let capture = CapturedAssurance::read(&core).map_err(io::Error::other)?;
        for (path, bytes) in capture.inputs() {
            let path = fixture.root.join("crates/core").join(path);
            fs::create_dir_all(
                path.parent()
                    .ok_or_else(|| io::Error::other("fixture parent"))?,
            )?;
            fs::write(path, bytes)?;
        }
        test(&mut fixture)
    })();
    match (operation, fixture.close()) {
        (result, Ok(())) => result,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; mutation fixture cleanup failed: {cleanup}"
        ))),
    }
}
