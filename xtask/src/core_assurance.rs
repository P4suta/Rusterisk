use crate::{process::ProcessError, reference::Step};
use std::time::Duration;
mod manifest;
mod miri;
mod mutation;
mod runner;
mod snapshot;
pub use crate::core_proofs::Execution;
pub use manifest::normalize_manifest;
pub use miri::{validate_miri_discovery, validate_miri_execution, MiriInventory};
pub use mutation::{validate_mutation_inventory, validate_mutation_outcomes, MutationInventory};
pub use runner::{execute, Task};

pub const NIGHTLY: &str = "nightly-2026-08-21";
pub const MUTATION_FILTER: &str = "(Call::(apply|claim_connection|reserve_stasis|enter_stasis|move_stasis|leave_stasis|join|leave)|MediaTasks::(reserve|index|cancel|finish)|ResourceSubscriptions.*::(subscribe|unsubscribe|expire))\\b";

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
}

impl Phase {
    pub const fn label(self) -> &'static str {
        match self {
            Self::MiriSetup => "miri-setup",
            Self::MiriInventory => "miri-inventory",
            Self::MiriTests => "miri-tests",
            Self::MutationInventory => "mutation-inventory",
            Self::Mutations => "mutations",
        }
    }
}

pub struct CoreAssurancePlan {
    identity: String,
    source: String,
    state: String,
    image: String,
}

impl CoreAssurancePlan {
    pub fn new(
        identity: &str,
        source: &str,
        state: &str,
        image: &str,
    ) -> Result<Self, AssuranceError> {
        let image_valid = image.rsplit_once("@sha256:").is_some_and(|(name, digest)| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".-_/:".contains(&byte))
                && digest.len() == 64
                && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
        if identity.is_empty()
            || identity.len() > 64
            || identity.starts_with('-')
            || !identity
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || image.len() > 512
            || !image_valid
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
            image: image.into(),
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
            &self.image,
            "--signal=TERM",
            "--kill-after=10s",
            "880s",
            "cargo",
            &format!("+{NIGHTLY}"),
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        match phase {
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
            budget: Duration::from_secs(900),
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
