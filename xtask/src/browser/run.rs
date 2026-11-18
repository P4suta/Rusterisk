use super::BrowserError;
use crate::reference::{valid_identity, ImageIdentity, Step};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    RustVersion,
    CargoVersion,
    NodeVersion,
    NpmVersion,
    Fetch,
    Install,
    BrowserVersion,
    Types,
    Check,
    Unit,
    Bundle,
    Compile,
    Inventory,
    Run,
}

impl Phase {
    pub const ALL: [Self; 14] = [
        Self::RustVersion,
        Self::CargoVersion,
        Self::NodeVersion,
        Self::NpmVersion,
        Self::Fetch,
        Self::Install,
        Self::BrowserVersion,
        Self::Types,
        Self::Check,
        Self::Unit,
        Self::Bundle,
        Self::Compile,
        Self::Inventory,
        Self::Run,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::RustVersion => "rust-version",
            Self::CargoVersion => "cargo-version",
            Self::NodeVersion => "node-version",
            Self::NpmVersion => "npm-version",
            Self::Fetch => "fetch",
            Self::Install => "install",
            Self::BrowserVersion => "browser-version",
            Self::Types => "types",
            Self::Check => "check",
            Self::Unit => "unit",
            Self::Bundle => "bundle",
            Self::Compile => "compile",
            Self::Inventory => "inventory",
            Self::Run => "run",
        }
    }

    pub const fn downloads(self) -> bool {
        matches!(self, Self::Fetch | Self::Install)
    }
}

pub struct RunPlan {
    identity: String,
    source: String,
    state: String,
    image: ImageIdentity,
}

impl RunPlan {
    #[cfg(unix)]
    pub(super) fn identity(&self) -> &str {
        &self.identity
    }

    #[cfg(unix)]
    pub(super) fn source(&self) -> &str {
        &self.source
    }

    #[cfg(unix)]
    pub(super) fn state(&self) -> &str {
        &self.state
    }

    pub fn image(&self) -> &str {
        self.image.as_str()
    }

    pub fn new(
        identity: &str,
        source: &str,
        state: &str,
        image: &str,
    ) -> Result<Self, BrowserError> {
        let image = ImageIdentity::parse(image).ok_or(BrowserError::Plan)?;
        let path_valid = |path: &str| {
            path.len() <= 4096
                && (path.starts_with('/') || std::path::Path::new(path).is_absolute())
                && !path.chars().any(|value| value.is_control() || value == ',')
                && !path
                    .split('/')
                    .any(|component| matches!(component, "." | ".."))
        };
        if !valid_identity(identity) || !path_valid(source) || !path_valid(state) || source == state
        {
            return Err(BrowserError::Plan);
        }
        Ok(Self {
            identity: identity.into(),
            source: source.into(),
            state: state.into(),
            image,
        })
    }

    fn name(&self, phase: Phase) -> String {
        format!("rusterisk-browser-{}-{}", self.identity, phase.label())
    }

    pub fn step(&self, phase: Phase) -> Step {
        self.step_in(phase, None)
    }

    #[cfg(unix)]
    pub(super) fn reference_step(&self, id: &str) -> Result<Step, BrowserError> {
        if id.len() != 64
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(BrowserError::Plan);
        }
        Ok(self.step_in(Phase::Run, Some(id)))
    }

    fn step_in(&self, phase: Phase, reference: Option<&str>) -> Step {
        let file_size = if phase == Phase::Compile {
            "fsize=536870912:536870912"
        } else {
            "fsize=134217728:134217728"
        };
        let budget = match phase {
            Phase::Fetch | Phase::Compile => 900,
            Phase::Install | Phase::Types | Phase::Run => 180,
            Phase::Check => 120,
            Phase::RustVersion
            | Phase::CargoVersion
            | Phase::NodeVersion
            | Phase::NpmVersion
            | Phase::BrowserVersion
            | Phase::Unit
            | Phase::Bundle
            | Phase::Inventory => 30,
        };
        let network = match reference {
            Some(id) => format!("--network=container:{id}"),
            None if phase.downloads() => "--network=bridge".into(),
            None => "--network=none".into(),
        };
        let [cpus, memory, swap, report, artifacts] = if reference.is_some() {
            [
                "--cpus=1",
                "--memory=1536m",
                "--memory-swap=1536m",
                "--env=PLAYWRIGHT_JSON_OUTPUT_FILE=/state/reference-browser-results.json",
                "--env=RUSTERISK_BROWSER_ARTIFACTS=/state/reference-browser-artifacts",
            ]
        } else {
            [
                "--cpus=2",
                "--memory=2g",
                "--memory-swap=2g",
                "--env=PLAYWRIGHT_JSON_OUTPUT_FILE=/state/browser-results.json",
                "--env=RUSTERISK_BROWSER_ARTIFACTS=/state/browser-artifacts",
            ]
        };
        let mut arguments: Vec<String> = [
            "run",
            "--rm",
            "--pull=never",
            "--name",
            &self.name(phase),
            cpus,
            memory,
            swap,
            if phase == Phase::Run {
                "--pids-limit=512"
            } else {
                "--pids-limit=256"
            },
            &network,
            "--read-only",
            "--init",
            "--shm-size=512m",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--ulimit",
            file_size,
            "--tmpfs",
            "/tmp:mode=1777,size=128m",
            "--mount",
            &format!("type=bind,source={},target=/project,readonly", self.source),
            "--mount",
            &format!("type=bind,source={},target=/state", self.state),
            "--mount",
            &format!(
                "type=bind,source={}/browser,target=/project/examples/browser",
                self.state
            ),
            "--workdir=/project",
            "--env=CARGO_HOME=/state/cargo",
            "--env=CARGO_TARGET_DIR=/state/target",
            "--env=CARGO_BUILD_JOBS=1",
            "--env=CARGO_PROFILE_DEV_DEBUG=line-tables-only",
            "--env=CARGO_PROFILE_DEV_INCREMENTAL=false",
            if phase.downloads() {
                "--env=CARGO_NET_OFFLINE=false"
            } else {
                "--env=CARGO_NET_OFFLINE=true"
            },
            "--env=HOME=/state/home",
            "--env=XDG_CACHE_HOME=/state/cache",
            "--env=TMPDIR=/state/tmp",
            report,
            artifacts,
            "--entrypoint=/usr/bin/timeout",
            self.image.as_str(),
            "--signal=TERM",
            "--kill-after=10s",
            &format!("{}s", budget - 20),
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        if reference.is_some() {
            arguments.insert(1, "--env=RUSTERISK_BROWSER_SERVER=asterisk".into());
        }
        let package = ["--prefix", "examples/browser"];
        match phase {
            Phase::RustVersion => arguments.extend(["rustc", "-vV"].map(str::to_owned)),
            Phase::CargoVersion => arguments.extend(["cargo", "-vV"].map(str::to_owned)),
            Phase::NodeVersion => arguments.extend(["node", "--version"].map(str::to_owned)),
            Phase::NpmVersion => arguments.extend(["npm", "--version"].map(str::to_owned)),
            Phase::Fetch => arguments.extend(["cargo", "fetch", "--locked"].map(str::to_owned)),
            Phase::Install => arguments.extend(
                [
                    "npm",
                    package[0],
                    package[1],
                    "ci",
                    "--ignore-scripts",
                    "--no-audit",
                    "--no-fund",
                ]
                .map(str::to_owned),
            ),
            Phase::BrowserVersion => arguments.extend(
                [
                    "node",
                    "examples/browser/node_modules/@playwright/test/cli.js",
                    "--version",
                ]
                .map(str::to_owned),
            ),
            Phase::Types => arguments.extend(
                [
                    "cargo",
                    "run",
                    "--locked",
                    "--offline",
                    "--package",
                    "xtask",
                    "--",
                    "browser-types",
                ]
                .map(str::to_owned),
            ),
            Phase::Check => {
                arguments.extend(["npm", package[0], package[1], "run", "check"].map(str::to_owned))
            }
            Phase::Unit => {
                arguments.extend(["npm", package[0], package[1], "test"].map(str::to_owned))
            }
            Phase::Bundle => {
                arguments.extend(["npm", package[0], package[1], "run", "build"].map(str::to_owned))
            }
            Phase::Compile => arguments.extend(
                [
                    "cargo",
                    "test",
                    "--locked",
                    "--offline",
                    "--package",
                    "rusterisk-server",
                    "--test",
                    "browser_calls",
                    "--no-run",
                ]
                .map(str::to_owned),
            ),
            Phase::Inventory => arguments.extend(
                [
                    "node",
                    "examples/browser/node_modules/@playwright/test/cli.js",
                    "test",
                    "--config=examples/browser/playwright.config.ts",
                    "--list",
                    "--reporter=json",
                ]
                .map(str::to_owned),
            ),
            Phase::Run => arguments.extend(
                [
                    "cargo",
                    "test",
                    "--locked",
                    "--offline",
                    "--package",
                    "rusterisk-server",
                    "--test",
                    "browser_calls",
                    "--",
                    "--include-ignored",
                    "--test-threads=1",
                    "--nocapture",
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
                .into_iter()
                .map(str::to_owned)
                .collect(),
            budget: Duration::from_secs(30),
        }
    }
}

#[cfg(all(test, unix))]
mod reference_tests {
    use super::*;

    #[test]
    fn comparison_joins_the_verified_container_only_for_native_browser_use(
    ) -> Result<(), BrowserError> {
        let image = format!("sha256:{}", "b".repeat(64));
        let id = "a".repeat(64);
        let plan = RunPlan::new("compare", "/source", "/state", &image)?;
        let reference = plan.reference_step(&id)?;
        for expected in [
            format!("--network=container:{id}"),
            "--cpus=1".to_owned(),
            "--memory=1536m".to_owned(),
            "--env=RUSTERISK_BROWSER_SERVER=asterisk".to_owned(),
            "--env=PLAYWRIGHT_JSON_OUTPUT_FILE=/state/reference-browser-results.json".to_owned(),
            "--env=RUSTERISK_BROWSER_ARTIFACTS=/state/reference-browser-artifacts".to_owned(),
        ] {
            assert!(reference.arguments.contains(&expected));
        }
        assert_eq!(
            reference
                .arguments
                .iter()
                .filter(|arg| arg.starts_with("--network="))
                .count(),
            1
        );
        assert!(plan.reference_step("another-container").is_err());
        assert!(plan
            .step(Phase::Run)
            .arguments
            .iter()
            .any(|argument| argument == "--network=none"));
        assert!(plan
            .step(Phase::Compile)
            .arguments
            .iter()
            .any(|argument| argument == "--network=none"));
        Ok(())
    }
}
