use crate::{process::ProcessError, reference::Step};
use std::time::Duration;
mod field_mutations;
mod field_runner;
mod manifest;
pub use field_mutations::FieldMutation;
mod captured;
mod captured_tree;
mod miri;
mod mutation;
mod runner;
mod snapshot;
pub use crate::core_proofs::Execution;
pub(crate) use captured::CapturedAssurance;
pub(crate) use captured_tree::{AssuranceTreeError, CapturedAssuranceTree};
pub use manifest::normalize_manifest;
pub use miri::{
    validate_field_execution, validate_miri_discovery, validate_miri_execution, MiriInventory,
};
pub use mutation::{validate_mutation_inventory, validate_mutation_outcomes, MutationInventory};
pub use runner::{execute, Task};

pub(crate) fn validate_mutation_campaign(
    bytes: &[u8],
    inventory: &MutationInventory,
    assignments: &[FieldMutation],
) -> Result<(), AssuranceError> {
    mutation::validate_mutation_campaign(bytes, inventory, assignments)
}

pub const NIGHTLY: &str = "nightly-2026-08-21";
pub const MUTATION_FILTER: &str = "(Call::(apply|claim_connection|reserve_stasis|enter_stasis|move_stasis|leave_stasis|join|leave)|MediaTasks::(reserve|index|cancel|finish)|ResourceSubscriptions.*::(subscribe|unsubscribe|expire)|Transfer::(begin|finish|participant_ended)|AttendedTransfer::(begin|finish|participant_ended)|OwnedCallPair::(new|participant_ended|retire)|SettingsLifetimes::(slot|available|prepare|cancel|commit|retain|retain_unpublished|release)|SettingsError::into_reference|CallTable.*::(available|admit|teardown|await_media|acknowledge)|Teardown.*::next)\\b";

#[derive(Debug)]
pub enum AssuranceError {
    Plan,
    Manifest,
    Syntax(syn::Error),
    Inventory,
    EmptyMutationInventory,
    MissingMutationScopes(Vec<&'static str>),
    UndetectedMutationScopes(Vec<&'static str>),
    UnknownMutationScope {
        file: String,
        function: Option<String>,
    },
    Results,
    Io(std::io::Error),
    Json(serde_json::Error),
    Process(ProcessError),
    Cleanup {
        operation: String,
        cleanup: ProcessError,
    },
}

impl std::fmt::Display for AssuranceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plan => formatter.write_str("invalid bounded core assurance plan"),
            Self::Manifest => formatter.write_str("core assurance must include the actual source and every integration test"),
            Self::Syntax(error) => error.fmt(formatter),
            Self::Inventory => formatter.write_str("core assurance requires a nonempty complete native inventory"),
            Self::EmptyMutationInventory => formatter.write_str("native mutation inventory is empty"),
            Self::MissingMutationScopes(scopes) => write!(formatter, "native mutation inventory is missing required invariants: {}", scopes.join(", ")),
            Self::UndetectedMutationScopes(scopes) => write!(formatter, "native mutation results have no caught viable mutation for required invariants: {}", scopes.join(", ")),
            Self::UnknownMutationScope { file, function } => match function {
                Some(function) => write!(formatter, "native mutation inventory has an unrecognized scope: {file}: {function}"),
                None => write!(formatter, "native mutation inventory has no enclosing function: {file}"),
            },
            Self::Results => formatter.write_str("core assurance requires complete successful results without skipped tests, missed mutations, or timeouts"),
            Self::Io(error) => error.fmt(formatter),
            Self::Json(error) => error.fmt(formatter),
            Self::Process(error) => error.fmt(formatter),
            Self::Cleanup { operation, cleanup } => write!(formatter, "{operation}; assurance cleanup failed: {cleanup}"),
        }
    }
}
impl std::error::Error for AssuranceError {}
impl From<std::io::Error> for AssuranceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<ProcessError> for AssuranceError {
    fn from(error: ProcessError) -> Self {
        Self::Process(error)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    MiriSetup,
    MiriInventory,
    MiriTests,
    MutationInventory,
    Mutations,
    FieldBuild(FieldMutation),
    FieldTests(FieldMutation),
}

impl Phase {
    pub const fn label(self) -> &'static str {
        match self {
            Self::MiriSetup => "miri-setup",
            Self::MiriInventory => "miri-inventory",
            Self::MiriTests => "miri-tests",
            Self::MutationInventory => "mutation-inventory",
            Self::Mutations => "mutations",
            Self::FieldBuild(FieldMutation::TerminalState) => "terminal-state-build",
            Self::FieldTests(FieldMutation::TerminalState) => "terminal-state-tests",
            Self::FieldBuild(FieldMutation::ConnectionClaim) => "connection-claim-build",
            Self::FieldTests(FieldMutation::ConnectionClaim) => "connection-claim-tests",
        }
    }
}

pub struct CoreAssurancePlan {
    identity: String,
    source: String,
    state: String,
    image: crate::reference::ImageIdentity,
}

impl CoreAssurancePlan {
    pub fn new(
        identity: &str,
        source: &str,
        state: &str,
        image: &str,
    ) -> Result<Self, AssuranceError> {
        let image = crate::reference::ImageIdentity::parse(image).ok_or(AssuranceError::Plan)?;
        if identity.is_empty()
            || identity.len() > 64
            || identity.starts_with('-')
            || !identity
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || [source, state].iter().any(|path| {
                path.is_empty() || path.len() > 4096 || path.contains([',', '\r', '\n', '\0'])
            })
        {
            return Err(AssuranceError::Plan);
        }
        Ok(Self {
            identity: identity.into(),
            source: source.into(),
            state: state.into(),
            image,
        })
    }

    fn name(&self, phase: Phase) -> String {
        format!(
            "rusterisk-core-assurance-{}-{}",
            self.identity,
            phase.label()
        )
    }

    pub fn step(&self, phase: Phase) -> Step {
        let budget = match phase {
            Phase::FieldBuild(_) | Phase::FieldTests(_) => 60,
            Phase::MiriSetup
            | Phase::MiriInventory
            | Phase::MiriTests
            | Phase::MutationInventory
            | Phase::Mutations => 900,
        };
        let network = if phase == Phase::MiriSetup {
            "--network=bridge"
        } else {
            "--network=none"
        };
        let offline = if phase == Phase::MiriSetup {
            "--env=CARGO_NET_OFFLINE=false"
        } else {
            "--env=CARGO_NET_OFFLINE=true"
        };
        let mut arguments: Vec<String> = [
            "run",
            "--rm",
            "--pull=never",
            "--name",
            &self.name(phase),
            "--cpus=2",
            "--memory=2g",
            "--memory-swap=2g",
            "--pids-limit=256",
            network,
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--ulimit",
            "fsize=134217728:134217728",
            "--tmpfs",
            "/tmp:mode=1777,size=128m",
            "--mount",
            &format!("type=bind,source={},target=/project,readonly", self.source),
            "--mount",
            &format!("type=bind,source={},target=/state", self.state),
            "--workdir=/project",
            "--env=CARGO_HOME=/state/cargo",
            "--env=CARGO_BUILD_JOBS=1",
            "--env=CARGO_TARGET_DIR=/state/target",
            "--env=XDG_CACHE_HOME=/state/cache",
            "--env=TMPDIR=/state/tmp",
            offline,
            "--env=MIRI_SYSROOT=/state/miri",
            "--env=MIRIFLAGS=-Zmiri-seed=1",
            "--entrypoint=/usr/bin/timeout",
            self.image.as_str(),
            "--signal=TERM",
            "--kill-after=10s",
            &format!("{}s", budget - 20),
            "cargo",
            &format!("+{NIGHTLY}"),
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        match phase {
            Phase::FieldBuild(_) => arguments.extend(
                ["test", "--locked", "--offline", "--tests", "--no-run"].map(str::to_owned),
            ),
            Phase::FieldTests(_) => arguments.extend(
                [
                    "test",
                    "--locked",
                    "--offline",
                    "--tests",
                    "--no-fail-fast",
                    "--",
                    "--test-threads=1",
                    "--format=json",
                    "-Z",
                    "unstable-options",
                ]
                .map(str::to_owned),
            ),
            Phase::MiriSetup => arguments.extend(["miri", "setup"].map(str::to_owned)),
            Phase::MiriInventory | Phase::MiriTests => {
                arguments.extend(
                    [
                        "miri",
                        "test",
                        "--locked",
                        "--offline",
                        "--tests",
                        "--",
                        "--test-threads=1",
                        "--format=json",
                        "-Z",
                        "unstable-options",
                    ]
                    .map(str::to_owned),
                );
                if phase == Phase::MiriInventory {
                    arguments.push("--list".into());
                }
            }
            Phase::MutationInventory | Phase::Mutations => {
                arguments.extend(
                    [
                        "mutants",
                        "--no-config",
                        "--file",
                        "src/lib.rs",
                        "--file",
                        "src/stasis.rs",
                        "--file",
                        "src/media_tasks.rs",
                        "--file",
                        "src/resource_subscriptions.rs",
                        "--file",
                        "src/transfer.rs",
                        "--file",
                        "src/attended_transfer.rs",
                        "--file",
                        "src/owned_call_pair.rs",
                        "--file",
                        "src/call_table.rs",
                        "--file",
                        "src/settings_lifetimes.rs",
                        "--re",
                        MUTATION_FILTER,
                    ]
                    .map(str::to_owned),
                );
                if phase == Phase::MutationInventory {
                    arguments.extend(["--list", "--json"].map(str::to_owned));
                } else {
                    arguments.extend(
                        [
                            "--jobs=1",
                            "--jobserver-tasks=1",
                            "--baseline=run",
                            "--timeout=30",
                            "--build-timeout=60",
                            "--no-shuffle",
                            "--cap-lints=false",
                            "--output=/state/results",
                        ]
                        .map(str::to_owned),
                    );
                }
            }
        }
        Step {
            arguments,
            budget: Duration::from_secs(budget),
        }
    }

    pub fn cleanup(&self, phase: Phase) -> Step {
        Step {
            arguments: ["rm", "--force", &self.name(phase)]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            budget: Duration::from_secs(30),
        }
    }
}
