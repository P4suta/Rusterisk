use super::{ServerBuildPhase, ServerBuildPlan, ServerTarget};
use crate::reference::{valid_identity, ImageIdentity, Step};
use std::{io, path::Path, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerBuildCommandMode {
    Work,
    Cleanup,
}

pub struct ServerContainerBuildPlan {
    name: String,
    source: String,
    state: String,
    image: ImageIdentity,
    platform: &'static str,
    cargo: ServerBuildPlan,
}

impl ServerContainerBuildPlan {
    pub fn new(
        identity: &str,
        source: &Path,
        state: &Path,
        target: ServerTarget,
        image: &str,
    ) -> io::Result<Self> {
        let platform = match target {
            ServerTarget::LinuxX86_64 => "linux/amd64",
            ServerTarget::LinuxArm64 => "linux/arm64",
            ServerTarget::MacOsArm64 | ServerTarget::MacOsX86_64 | ServerTarget::WindowsX86_64 => {
                return Err(io::Error::other(
                    "container server builds require a Linux target",
                ));
            }
        };
        let image = ImageIdentity::parse(image)
            .ok_or_else(|| io::Error::other("server build image requires an immutable digest"))?;
        if !valid_identity(identity) {
            return Err(io::Error::other("server build identity is invalid"));
        }
        ServerBuildPlan::new(source, state, target)?;
        let mount = |path: &Path| {
            path.to_str()
                .filter(|path| !path.contains(','))
                .map(str::to_owned)
                .ok_or_else(|| {
                    io::Error::other("server build mounts require unambiguous UTF-8 paths")
                })
        };
        Ok(Self {
            name: format!("rusterisk-server-build-{identity}"),
            source: mount(source)?,
            state: mount(state)?,
            image,
            platform,
            cargo: ServerBuildPlan::new(Path::new("/source"), Path::new("/state"), target)?,
        })
    }

    pub fn step(&self, phase: ServerBuildPhase) -> Step {
        let cargo = self.cargo.step(phase);
        let mut arguments = [
            "run",
            "--rm",
            "--cpus=2",
            "--memory=2g",
            "--memory-swap=2g",
            "--pids-limit=256",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--name",
        ]
        .map(str::to_owned)
        .to_vec();
        arguments.push(format!("{}-{}", self.name, phase.label()));
        arguments.extend([
            "--platform".to_owned(),
            self.platform.to_owned(),
            "--mount".to_owned(),
            format!("type=bind,source={},target=/source,readonly", self.source),
            "--mount".to_owned(),
            format!("type=bind,source={},target=/state", self.state),
            "--workdir=/source".to_owned(),
        ]);
        for environment in [
            "CARGO_HOME=/state/cargo",
            "CARGO_TARGET_DIR=/state/target",
            "CARGO_BUILD_JOBS=2",
            "TMPDIR=/state/tmp",
            "RUSTUP_TOOLCHAIN=1.99.0",
            match phase {
                ServerBuildPhase::Fetch => "CARGO_NET_OFFLINE=false",
                ServerBuildPhase::Build => "CARGO_NET_OFFLINE=true",
            },
        ] {
            arguments.extend(["--env".to_owned(), environment.to_owned()]);
        }
        match phase {
            ServerBuildPhase::Fetch => {}
            ServerBuildPhase::Build => arguments.push("--network=none".to_owned()),
        }
        arguments.extend([self.image.as_str().to_owned(), "cargo".to_owned()]);
        arguments.extend(cargo.arguments);
        Step {
            arguments,
            budget: cargo.budget,
        }
    }

    pub fn cleanup(&self, phase: ServerBuildPhase) -> Step {
        Step {
            arguments: [
                "rm".to_owned(),
                "--force".to_owned(),
                format!("{}-{}", self.name, phase.label()),
            ]
            .into(),
            budget: Duration::from_secs(30),
        }
    }
}
