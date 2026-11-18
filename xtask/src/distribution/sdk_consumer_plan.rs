use crate::reference::Step;
use std::{
    num::NonZeroUsize,
    path::{Component, Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SdkConsumerScope {
    CoreMsrv,
    Sdk,
}

impl SdkConsumerScope {
    pub const ALL: [Self; 2] = [Self::CoreMsrv, Self::Sdk];

    pub const fn label(self) -> &'static str {
        match self {
            Self::CoreMsrv => "core-msrv",
            Self::Sdk => "sdk",
        }
    }

    pub const fn package_name(self) -> &'static str {
        match self {
            Self::CoreMsrv => "rusterisk-core-candidate-consumer",
            Self::Sdk => "rusterisk-sdk-candidate-consumer",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SdkConsumerAction {
    Resolve,
    Metadata,
    Build,
    Run,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SdkConsumerPhase {
    scope: SdkConsumerScope,
    action: SdkConsumerAction,
}

impl SdkConsumerPhase {
    pub const ALL: [Self; 8] = [
        Self::new(SdkConsumerScope::CoreMsrv, SdkConsumerAction::Resolve),
        Self::new(SdkConsumerScope::CoreMsrv, SdkConsumerAction::Metadata),
        Self::new(SdkConsumerScope::CoreMsrv, SdkConsumerAction::Build),
        Self::new(SdkConsumerScope::CoreMsrv, SdkConsumerAction::Run),
        Self::new(SdkConsumerScope::Sdk, SdkConsumerAction::Resolve),
        Self::new(SdkConsumerScope::Sdk, SdkConsumerAction::Metadata),
        Self::new(SdkConsumerScope::Sdk, SdkConsumerAction::Build),
        Self::new(SdkConsumerScope::Sdk, SdkConsumerAction::Run),
    ];

    pub const fn new(scope: SdkConsumerScope, action: SdkConsumerAction) -> Self {
        Self { scope, action }
    }
    pub const fn scope(self) -> SdkConsumerScope {
        self.scope
    }
    pub const fn action(self) -> SdkConsumerAction {
        self.action
    }

    pub fn label(self) -> String {
        let action = match self.action {
            SdkConsumerAction::Resolve => "resolve",
            SdkConsumerAction::Metadata => "metadata",
            SdkConsumerAction::Build => "build",
            SdkConsumerAction::Run => "run",
        };
        format!("{}-{action}", self.scope.label())
    }
}

struct Paths {
    directory: PathBuf,
    target: PathBuf,
    target_argument: String,
    executable: String,
}

impl Paths {
    fn new(work: &Path, state: &Path, scope: SdkConsumerScope) -> Result<Self, std::io::Error> {
        let invalid = || std::io::Error::other("SDK consumer paths must be UTF-8");
        let target = state.join("target").join(scope.label());
        Ok(Self {
            directory: work.join("consumers").join(scope.label()),
            target_argument: target.to_str().ok_or_else(invalid)?.to_owned(),
            executable: target
                .join("debug")
                .join(format!(
                    "{}{}",
                    scope.package_name(),
                    std::env::consts::EXE_SUFFIX
                ))
                .to_str()
                .ok_or_else(invalid)?
                .to_owned(),
            target,
        })
    }
}

pub struct SdkConsumerPlan {
    work: PathBuf,
    state: PathBuf,
    jobs: NonZeroUsize,
    core: Paths,
    sdk: Paths,
}

fn absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .to_str()
            .is_some_and(|path| path.len() <= 4096 && !path.chars().any(char::is_control))
        && !path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
}

impl SdkConsumerPlan {
    pub const DEFAULT_JOBS: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);

    pub fn new(work: &Path, state: &Path) -> Result<Self, std::io::Error> {
        Self::new_with_jobs(work, state, Self::DEFAULT_JOBS)
    }

    pub fn new_with_jobs(
        work: &Path,
        state: &Path,
        jobs: NonZeroUsize,
    ) -> Result<Self, std::io::Error> {
        if !absolute(work) || !absolute(state) || work.starts_with(state) || state.starts_with(work)
        {
            return Err(std::io::Error::other(
                "SDK consumers require separate absolute work and state paths",
            ));
        }
        Ok(Self {
            work: work.to_owned(),
            state: state.to_owned(),
            jobs,
            core: Paths::new(work, state, SdkConsumerScope::CoreMsrv)?,
            sdk: Paths::new(work, state, SdkConsumerScope::Sdk)?,
        })
    }

    pub fn work(&self) -> &Path {
        &self.work
    }
    pub fn state(&self) -> &Path {
        &self.state
    }
    pub const fn jobs(&self) -> NonZeroUsize {
        self.jobs
    }

    fn paths(&self, scope: SdkConsumerScope) -> &Paths {
        match scope {
            SdkConsumerScope::CoreMsrv => &self.core,
            SdkConsumerScope::Sdk => &self.sdk,
        }
    }

    pub fn directory(&self, scope: SdkConsumerScope) -> &Path {
        &self.paths(scope).directory
    }
    pub fn target(&self, scope: SdkConsumerScope) -> &Path {
        &self.paths(scope).target
    }

    pub fn program(&self, phase: SdkConsumerPhase) -> &str {
        match (phase.scope, phase.action) {
            (scope, SdkConsumerAction::Run) => &self.paths(scope).executable,
            (
                SdkConsumerScope::CoreMsrv,
                SdkConsumerAction::Resolve | SdkConsumerAction::Metadata | SdkConsumerAction::Build,
            ) => "rustup",
            (
                SdkConsumerScope::Sdk,
                SdkConsumerAction::Resolve | SdkConsumerAction::Metadata | SdkConsumerAction::Build,
            ) => "cargo",
        }
    }

    pub fn step(&self, phase: SdkConsumerPhase) -> Step {
        let mut arguments = Vec::new();
        if phase.scope == SdkConsumerScope::CoreMsrv && phase.action != SdkConsumerAction::Run {
            arguments.extend(["run", "1.89.0", "cargo"].map(str::to_owned));
        }
        let seconds = match phase.action {
            SdkConsumerAction::Resolve => {
                arguments
                    .extend(["metadata", "--format-version", "1", "--offline"].map(str::to_owned));
                180
            }
            SdkConsumerAction::Metadata => {
                arguments.extend(
                    ["metadata", "--format-version", "1", "--offline", "--locked"]
                        .map(str::to_owned),
                );
                60
            }
            SdkConsumerAction::Build => {
                let jobs = self.jobs.to_string();
                arguments.extend(
                    [
                        "build",
                        "--locked",
                        "--offline",
                        "--jobs",
                        &jobs,
                        "--target-dir",
                    ]
                    .map(str::to_owned),
                );
                arguments.push(self.paths(phase.scope).target_argument.clone());
                900
            }
            SdkConsumerAction::Run => 30,
        };
        Step {
            arguments,
            budget: Duration::from_secs(seconds),
        }
    }
}
