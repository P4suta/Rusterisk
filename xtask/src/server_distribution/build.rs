use super::ServerTarget;
use crate::process::cargo_path;
use crate::reference::Step;
use std::{
    io,
    num::{NonZeroU16, NonZeroUsize},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeServerBuildLimits {
    jobs: NonZeroUsize,
    build_timeout_seconds: NonZeroU16,
}

impl NativeServerBuildLimits {
    pub fn new(jobs: usize, build_timeout_seconds: u16) -> io::Result<Self> {
        let jobs = NonZeroUsize::new(jobs)
            .ok_or_else(|| io::Error::other("native server build jobs must be positive"))?;
        let build_timeout_seconds = NonZeroU16::new(build_timeout_seconds)
            .ok_or_else(|| io::Error::other("native server build timeout must be positive"))?;
        let limits = Self {
            jobs,
            build_timeout_seconds,
        };
        if Instant::now().checked_add(limits.build_timeout()).is_none() {
            return Err(io::Error::other(
                "native server build deadline exceeds the platform clock",
            ));
        }
        Ok(limits)
    }

    pub(super) const fn jobs(self) -> NonZeroUsize {
        self.jobs
    }

    fn build_timeout(self) -> Duration {
        Duration::from_secs(u64::from(self.build_timeout_seconds.get()))
    }
}

impl Default for NativeServerBuildLimits {
    fn default() -> Self {
        Self {
            jobs: NonZeroUsize::MIN.saturating_add(1),
            build_timeout_seconds: NonZeroU16::MIN.saturating_add(899),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerBuildPhase {
    Fetch,
    Build,
}

impl ServerBuildPhase {
    pub const ALL: [Self; 2] = [Self::Fetch, Self::Build];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::Build => "build",
        }
    }
}

pub struct ServerBuildPlan {
    source: PathBuf,
    state: PathBuf,
    target: ServerTarget,
    target_directory: String,
    limits: NativeServerBuildLimits,
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

impl ServerBuildPlan {
    pub fn new(source: &Path, state: &Path, target: ServerTarget) -> io::Result<Self> {
        Self::new_with_limits(source, state, target, NativeServerBuildLimits::default())
    }

    pub fn new_with_limits(
        source: &Path,
        state: &Path,
        target: ServerTarget,
        limits: NativeServerBuildLimits,
    ) -> io::Result<Self> {
        if !absolute(source)
            || !absolute(state)
            || source.starts_with(state)
            || state.starts_with(source)
        {
            return Err(io::Error::other(
                "server builds require separate absolute source and state paths",
            ));
        }
        let target_path = state.join("target");
        let target_directory = cargo_path(&target_path)?
            .to_str()
            .ok_or_else(|| io::Error::other("server target directory is not UTF-8"))?
            .to_owned();
        Ok(Self {
            source: source.to_owned(),
            state: state.to_owned(),
            target,
            target_directory,
            limits,
        })
    }

    pub fn source(&self) -> &Path {
        &self.source
    }
    pub fn state(&self) -> &Path {
        &self.state
    }

    pub(super) fn target_directory(&self) -> &str {
        &self.target_directory
    }
    pub const fn target(&self) -> ServerTarget {
        self.target
    }

    pub(super) const fn limits(&self) -> NativeServerBuildLimits {
        self.limits
    }

    pub fn executable(&self) -> PathBuf {
        self.state
            .join("target")
            .join(self.target.triple())
            .join("release")
            .join(match self.target {
                ServerTarget::WindowsX86_64 => "rusterisk.exe",
                ServerTarget::LinuxX86_64
                | ServerTarget::LinuxArm64
                | ServerTarget::MacOsArm64
                | ServerTarget::MacOsX86_64 => "rusterisk",
            })
    }

    pub fn step(&self, phase: ServerBuildPhase) -> Step {
        let jobs = self.limits.jobs().to_string();
        let (arguments, seconds) = match phase {
            ServerBuildPhase::Fetch => (
                ["fetch", "--locked", "--target", self.target.triple()]
                    .map(str::to_owned)
                    .to_vec(),
                180,
            ),
            ServerBuildPhase::Build => (
                [
                    "build",
                    "--locked",
                    "--offline",
                    "--release",
                    "--package",
                    "rusterisk-server",
                    "--package",
                    "rusterisk-cli",
                    "--bin",
                    "rusterisk",
                    "--bin",
                    "rusterisk-cli",
                    "--target",
                    self.target.triple(),
                    "--jobs",
                    &jobs,
                    "--target-dir",
                    &self.target_directory,
                ]
                .map(str::to_owned)
                .to_vec(),
                self.limits.build_timeout().as_secs(),
            ),
        };
        Step {
            arguments,
            budget: Duration::from_secs(seconds),
        }
    }
}
