use super::ServerTarget;
use crate::reference::Step;
use std::{
    io,
    path::{Component, Path, PathBuf},
    time::Duration,
};

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
        if !absolute(source)
            || !absolute(state)
            || source.starts_with(state)
            || state.starts_with(source)
        {
            return Err(io::Error::other(
                "server builds require separate absolute source and state paths",
            ));
        }
        let target_directory = state
            .join("target")
            .to_str()
            .ok_or_else(|| io::Error::other("server target directory is not UTF-8"))?
            .to_owned();
        Ok(Self {
            source: source.to_owned(),
            state: state.to_owned(),
            target,
            target_directory,
        })
    }

    pub fn source(&self) -> &Path {
        &self.source
    }
    pub fn state(&self) -> &Path {
        &self.state
    }
    pub const fn target(&self) -> ServerTarget {
        self.target
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
                    "--bin",
                    "rusterisk",
                    "--target",
                    self.target.triple(),
                    "--jobs",
                    "2",
                    "--target-dir",
                    &self.target_directory,
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
}
