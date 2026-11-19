use super::BrowserError;
use crate::reference::{valid_identity, ImageIdentity, Step};
use std::time::Duration;

const BROWSER_OUTPUTS: [&str; 2] = [
    "--env=PLAYWRIGHT_JSON_OUTPUT_FILE=/state/browser-results.json",
    "--env=RUSTERISK_BROWSER_ARTIFACTS=/state/browser-artifacts",
];

struct ExternalBrowser<'a> {
    id: &'a str,
    server: &'static str,
    outputs: [&'static str; 2],
}

#[cfg(unix)]
enum ExternalServer {
    Reference,
    Packaged,
    MatchedReference,
}

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
        self.external_step(id, ExternalServer::Reference)
    }

    #[cfg(unix)]
    pub(super) fn packaged_step(&self, id: &str) -> Result<Step, BrowserError> {
        self.external_step(id, ExternalServer::Packaged)
    }

    #[cfg(unix)]
    pub(super) fn matched_reference_step(&self, id: &str) -> Result<Step, BrowserError> {
        self.external_step(id, ExternalServer::MatchedReference)
    }

    #[cfg(unix)]
    fn external_step(&self, id: &str, target: ExternalServer) -> Result<Step, BrowserError> {
        if id.len() != 64
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(BrowserError::Plan);
        }
        let reference_outputs = [
            "--env=PLAYWRIGHT_JSON_OUTPUT_FILE=/state/reference-browser-results.json",
            "--env=RUSTERISK_BROWSER_ARTIFACTS=/state/reference-browser-artifacts",
        ];
        let (server, outputs) = match target {
            ExternalServer::Reference => {
                ("--env=RUSTERISK_BROWSER_SERVER=asterisk", reference_outputs)
            }
            ExternalServer::Packaged => (
                "--env=RUSTERISK_BROWSER_SERVER=packaged-rusterisk",
                BROWSER_OUTPUTS,
            ),
            ExternalServer::MatchedReference => (
                "--env=RUSTERISK_BROWSER_SERVER=packaged-asterisk",
                reference_outputs,
            ),
        };
        Ok(self.step_in(
            Phase::Run,
            Some(ExternalBrowser {
                id,
                server,
                outputs,
            }),
        ))
    }

    fn step_in(&self, phase: Phase, external: Option<ExternalBrowser<'_>>) -> Step {
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
        let network = match &external {
            Some(browser) => format!("--network=container:{}", browser.id),
            None if phase.downloads() => "--network=bridge".into(),
            None => "--network=none".into(),
        };
        let [cpus, memory, swap] = if external.is_some() {
            ["--cpus=1", "--memory=1536m", "--memory-swap=1536m"]
        } else {
            ["--cpus=2", "--memory=2g", "--memory-swap=2g"]
        };
        let [report, artifacts] = external
            .as_ref()
            .map_or(BROWSER_OUTPUTS, |browser| browser.outputs);
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
            if phase == Phase::Run {
                "--env=TMPDIR=/tmp"
            } else {
                "--env=TMPDIR=/state/tmp"
            },
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
        if let Some(browser) = external {
            arguments.insert(1, browser.server.into());
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
    fn every_native_backend_uses_tmpfs_and_keeps_its_selected_report() -> Result<(), BrowserError> {
        let image = format!("sha256:{}", "b".repeat(64));
        let id = "a".repeat(64);
        let plan = RunPlan::new("compare", "/source", "/state", &image)?;
        for (step, report) in [
            (plan.step(Phase::Run), BROWSER_OUTPUTS[0]),
            (plan.packaged_step(&id)?, BROWSER_OUTPUTS[0]),
            (
                plan.reference_step(&id)?,
                "--env=PLAYWRIGHT_JSON_OUTPUT_FILE=/state/reference-browser-results.json",
            ),
            (
                plan.matched_reference_step(&id)?,
                "--env=PLAYWRIGHT_JSON_OUTPUT_FILE=/state/reference-browser-results.json",
            ),
        ] {
            let temporary: Vec<_> = step
                .arguments
                .iter()
                .filter(|value| value.starts_with("--env=TMPDIR="))
                .map(String::as_str)
                .collect();
            assert_eq!(temporary, ["--env=TMPDIR=/tmp"]);
            assert_eq!(
                step.arguments
                    .iter()
                    .filter(|value| *value == report)
                    .count(),
                1
            );
        }
        Ok(())
    }

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

    #[test]
    fn packaged_comparisons_share_limits_and_save_both_reports() -> Result<(), BrowserError> {
        let image = format!("sha256:{}", "b".repeat(64));
        let id = "a".repeat(64);
        let plan = RunPlan::new("compare", "/source", "/state", &image)?;
        let packaged = plan.packaged_step(&id)?;
        let reference = plan.matched_reference_step(&id)?;
        for step in [&packaged, &reference] {
            for argument in ["--cpus=1", "--memory=1536m", "--memory-swap=1536m"] {
                assert!(step.arguments.iter().any(|present| present == argument));
            }
        }
        assert!(packaged
            .arguments
            .iter()
            .any(|argument| argument == "--env=RUSTERISK_BROWSER_SERVER=packaged-rusterisk"));
        assert!(reference
            .arguments
            .iter()
            .any(|argument| argument == "--env=RUSTERISK_BROWSER_SERVER=packaged-asterisk"));
        assert!(packaged
            .arguments
            .iter()
            .any(|argument| argument == BROWSER_OUTPUTS[0]));
        assert!(reference.arguments.iter().any(|argument| argument
            == "--env=PLAYWRIGHT_JSON_OUTPUT_FILE=/state/reference-browser-results.json"));
        Ok(())
    }
}
