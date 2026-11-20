use crate::process::{run, ProcessError};
use crate::reference::Step;
use std::{
    path::{Component, Path, PathBuf},
    process::Command,
    time::Duration,
};

pub(super) const FIXTURE_REGISTRY: &str = "rusterisk-sdk-fixtures";

#[derive(Debug)]
pub enum SdkBuildError {
    Io(std::io::Error),
    Plan,
    Process(ProcessError),
}

impl std::fmt::Display for SdkBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Process(error) => error.fmt(formatter),
            Self::Plan => {
                formatter.write_str("SDK builds require separate absolute source and state paths")
            }
        }
    }
}

impl std::error::Error for SdkBuildError {}

impl From<std::io::Error> for SdkBuildError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SdkBuildPhase {
    Fetch,
    Package,
}

impl SdkBuildPhase {
    pub const ALL: [Self; 2] = [Self::Fetch, Self::Package];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::Package => "package",
        }
    }
}

pub struct SdkBuildPlan {
    source: PathBuf,
    state: PathBuf,
    target: String,
}

fn absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .to_str()
            .is_some_and(|value| value.len() <= 4096 && !value.chars().any(char::is_control))
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
}

impl SdkBuildPlan {
    pub fn new(source: &Path, state: &Path) -> Result<Self, SdkBuildError> {
        if !absolute(source)
            || !absolute(state)
            || source.starts_with(state)
            || state.starts_with(source)
        {
            return Err(SdkBuildError::Plan);
        }
        Ok(Self {
            source: source.to_owned(),
            state: state.to_owned(),
            target: state
                .join("target")
                .to_str()
                .ok_or(SdkBuildError::Plan)?
                .to_owned(),
        })
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    pub fn step(&self, phase: SdkBuildPhase) -> Step {
        let (arguments, seconds) = match phase {
            SdkBuildPhase::Fetch => (["fetch", "--locked"].map(str::to_owned).to_vec(), 180),
            SdkBuildPhase::Package => (
                [
                    "package",
                    "--locked",
                    "--offline",
                    "--jobs",
                    "2",
                    "--package",
                    "rusterisk-core",
                    "--package",
                    "rusterisk-api",
                    "--package",
                    "rusterisk-client",
                    "--target-dir",
                    &self.target,
                    "--registry",
                    FIXTURE_REGISTRY,
                    "--config",
                    "registries.rusterisk-sdk-fixtures.index='sparse+https://index.crates.io/'",
                ]
                .map(str::to_owned)
                .to_vec(),
                900,
            ),
        };
        Step {
            arguments,
            budget: Duration::from_secs(seconds),
        }
    }

    pub fn execute_with(
        &self,
        mut perform: impl FnMut(&mut Command, Duration) -> Result<(), ProcessError>,
    ) -> Result<(), SdkBuildError> {
        for phase in SdkBuildPhase::ALL {
            let output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(self.state.join(format!("{}.txt", phase.label())))?;
            let step = self.step(phase);
            let mut command = Command::new("cargo");
            command
                .args(step.arguments)
                .current_dir(&self.source)
                .env("CARGO_TARGET_DIR", &self.target)
                .env("CARGO_BUILD_JOBS", "2")
                .env(
                    "CARGO_NET_OFFLINE",
                    if phase == SdkBuildPhase::Fetch {
                        "false"
                    } else {
                        "true"
                    },
                )
                .stdin(std::process::Stdio::null())
                .stdout(output.try_clone()?)
                .stderr(output);
            let started = std::time::Instant::now();
            let result = perform(&mut command, step.budget);
            println!(
                "SDK phase={} elapsed={:.3}s",
                phase.label(),
                started.elapsed().as_secs_f64()
            );
            result.map_err(SdkBuildError::Process)?;
        }
        Ok(())
    }

    pub fn execute(&self) -> Result<(), SdkBuildError> {
        self.execute_with(run)
    }
}
