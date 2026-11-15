use crate::{process::ProcessError, reference::Step};
use std::time::Duration;
mod manifest;
mod native;
mod runner;
mod snapshot;
#[path = "../fuzz/src/targets.rs"]
mod targets;
pub use crate::core_proofs::Execution;
pub use manifest::{normalize_manifest, validate_locks};
pub use native::{validate_inventory, validate_run};
pub use runner::{execute, Scope};
pub use targets::{Target, MAX_INPUT};

pub const NIGHTLY: &str = "nightly-2026-08-21";

#[derive(Debug)]
pub enum FuzzError {
    Plan,
    Manifest,
    Inventory,
    Results,
    Io(std::io::Error),
    Toml(toml::de::Error),
    Process(ProcessError),
    Cleanup {
        operation: String,
        cleanup: ProcessError,
    },
}

impl std::fmt::Display for FuzzError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plan => formatter.write_str("invalid bounded input fuzz plan"),
            Self::Manifest => formatter.write_str("fuzz manifest must bind every target to production workspace dependencies"),
            Self::Inventory => formatter.write_str("fuzz inventory must contain every maintained target exactly once"),
            Self::Results => formatter.write_str("input fuzz requires nonempty complete native execution without crashes or timeouts"),
            Self::Io(error) => error.fmt(formatter),
            Self::Toml(error) => error.fmt(formatter),
            Self::Process(error) => error.fmt(formatter),
            Self::Cleanup { operation, cleanup } => write!(formatter, "{operation}; input fuzz cleanup failed: {cleanup}"),
        }
    }
}

impl std::error::Error for FuzzError {}
impl From<std::io::Error> for FuzzError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<ProcessError> for FuzzError {
    fn from(error: ProcessError) -> Self {
        Self::Process(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinuxTarget {
    Arm64,
    Amd64,
}

impl LinuxTarget {
    pub const fn triple(self) -> &'static str {
        match self {
            Self::Arm64 => "aarch64-unknown-linux-gnu",
            Self::Amd64 => "x86_64-unknown-linux-gnu",
        }
    }

    pub fn host() -> Result<Self, FuzzError> {
        match std::env::consts::ARCH {
            "aarch64" => Ok(Self::Arm64),
            "x86_64" => Ok(Self::Amd64),
            _ => Err(FuzzError::Plan),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    RustVersion,
    Version,
    Inventory,
    Fetch,
    Build(Target),
    Run(Target),
}

impl Phase {
    pub fn label(self) -> String {
        match self {
            Self::RustVersion => "rust-version".into(),
            Self::Version => "cargo-fuzz-version".into(),
            Self::Inventory => "inventory".into(),
            Self::Fetch => "fetch".into(),
            Self::Build(target) => format!("build-{}", target.name()),
            Self::Run(target) => format!("run-{}", target.name()),
        }
    }
}

pub struct FuzzPlan {
    identity: String,
    source: String,
    state: String,
    image: String,
    target: LinuxTarget,
}

impl FuzzPlan {
    pub fn new(
        identity: &str,
        source: &str,
        state: &str,
        image: &str,
        target: LinuxTarget,
    ) -> Result<Self, FuzzError> {
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
            return Err(FuzzError::Plan);
        }
        Ok(Self {
            identity: identity.into(),
            source: source.into(),
            state: state.into(),
            image: image.into(),
            target,
        })
    }

    fn name(&self, phase: Phase) -> String {
        format!("rusterisk-input-fuzz-{}-{}", self.identity, phase.label())
    }

    pub fn step(&self, phase: Phase) -> Step {
        let budget = match phase {
            Phase::RustVersion | Phase::Version | Phase::Inventory => 30,
            Phase::Fetch | Phase::Build(_) => 900,
            Phase::Run(_) => 90,
        };
        let fetch = phase == Phase::Fetch;
        let mut arguments: Vec<String> = [
            "run", "--rm", "--pull=never", "--name", &self.name(phase),
            "--cpus=2", "--memory=2g", "--memory-swap=2g", "--pids-limit=256",
            if fetch { "--network=bridge" } else { "--network=none" },
            "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges",
            "--ulimit", "fsize=134217728:134217728",
            "--tmpfs", "/tmp:mode=1777,size=128m",
            "--mount", &format!("type=bind,source={},target=/project,readonly", self.source),
            "--mount", &format!("type=bind,source={},target=/state", self.state),
            "--workdir=/project",
            "--env=CARGO_HOME=/state/cargo",
            "--env=CARGO_TARGET_DIR=/state/target",
            "--env=CARGO_BUILD_JOBS=1",
            "--env=CARGO_PROFILE_DEV_DEBUG=line-tables-only",
            "--env=CARGO_PROFILE_DEV_INCREMENTAL=false",
            if fetch { "--env=CARGO_NET_OFFLINE=false" } else { "--env=CARGO_NET_OFFLINE=true" },
            "--env=TMPDIR=/state/tmp",
            "--env=ASAN_OPTIONS=detect_leaks=1:halt_on_error=1:abort_on_error=1:detect_odr_violation=1",
            "--entrypoint=/usr/bin/timeout", &self.image,
            "--signal=TERM", "--kill-after=10s", &format!("{}s", budget - 20),
        ].into_iter().map(str::to_owned).collect();
        match phase {
            Phase::RustVersion => {
                arguments.extend(["rustc", &format!("+{NIGHTLY}"), "-vV"].map(str::to_owned))
            }
            Phase::Version => arguments
                .extend(["cargo", &format!("+{NIGHTLY}"), "fuzz", "--version"].map(str::to_owned)),
            Phase::Inventory => arguments.extend(
                [
                    "cargo",
                    &format!("+{NIGHTLY}"),
                    "fuzz",
                    "list",
                    "--fuzz-dir=/state/fuzz",
                ]
                .map(str::to_owned),
            ),
            Phase::Fetch => arguments.extend(
                [
                    "cargo",
                    &format!("+{NIGHTLY}"),
                    "fetch",
                    "--locked",
                    "-Zbuild-std",
                    "--manifest-path=/state/fuzz/Cargo.toml",
                    &format!("--target={}", self.target.triple()),
                ]
                .map(str::to_owned),
            ),
            Phase::Build(target) => arguments.extend(
                [
                    "cargo",
                    &format!("+{NIGHTLY}"),
                    "fuzz",
                    "build",
                    "--fuzz-dir=/state/fuzz",
                    "--build-std",
                    "--sanitizer=address",
                    "--dev",
                    &format!("--target={}", self.target.triple()),
                    "--target-dir=/state/target",
                    &format!("--features={}", target.features()),
                    target.name(),
                ]
                .map(str::to_owned),
            ),
            Phase::Run(target) => arguments.extend(
                [
                    &format!(
                        "/state/target/{}/debug/{}",
                        self.target.triple(),
                        target.name()
                    ),
                    &format!("/state/corpus/{}", target.name()),
                    &format!("-artifact_prefix=/state/artifacts/{}/", target.name()),
                    "-seed=1",
                    "-runs=100000",
                    "-max_total_time=60",
                    "-max_len=65536",
                    "-timeout=2",
                    "-rss_limit_mb=1024",
                    "-print_final_stats=1",
                ]
                .map(str::to_owned),
            ),
        }
        Step {
            arguments,
            budget: Duration::from_secs(budget),
        }
    }

    pub fn cleanup(&self, phase: Phase) -> Step {
        Step {
            arguments: ["rm", "--force", &self.name(phase)]
                .map(str::to_owned)
                .into(),
            budget: Duration::from_secs(30),
        }
    }
}
