use crate::reference::{valid_identity, ImageIdentity, Step};
use std::{
    io,
    path::{Component, Path},
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerOciRunPhase {
    Version,
    PackagedConfig,
    RuntimeConfig,
    Serve,
}

impl ServerOciRunPhase {
    pub const ALL: [Self; 4] = [
        Self::Version,
        Self::PackagedConfig,
        Self::RuntimeConfig,
        Self::Serve,
    ];
    pub const fn label(self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::PackagedConfig => "packaged-config",
            Self::RuntimeConfig => "runtime-config",
            Self::Serve => "serve",
        }
    }
}

pub struct ServerOciRuntimePlan {
    name: String,
    image: ImageIdentity,
    configuration: String,
}

impl ServerOciRuntimePlan {
    pub fn new(identity: &str, image: &str, configuration: &Path) -> io::Result<Self> {
        if !valid_identity(identity)
            || !image.starts_with("sha256:")
            || image.bytes().any(|byte| byte.is_ascii_uppercase())
            || !configuration.is_absolute()
            || configuration
                .components()
                .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        {
            return Err(io::Error::other(
                "OCI runtime checks require owned names, exact image IDs, and absolute mounts",
            ));
        }
        let image = ImageIdentity::parse(image)
            .ok_or_else(|| io::Error::other("OCI runtime image ID is invalid"))?;
        let configuration = configuration
            .to_str()
            .filter(|path| {
                path.len() <= 4096 && !path.contains(',') && !path.chars().any(char::is_control)
            })
            .ok_or_else(|| io::Error::other("OCI runtime mount is ambiguous or too long"))?
            .to_owned();
        Ok(Self {
            name: format!("rusterisk-server-oci-check-{identity}"),
            image,
            configuration,
        })
    }

    fn container(&self, phase: ServerOciRunPhase) -> String {
        format!("{}-{}", self.name, phase.label())
    }

    pub fn step(&self, phase: ServerOciRunPhase) -> Step {
        let mut arguments: Vec<String> = [
            "run",
            "--rm",
            "--pull=never",
            "--cpus=2",
            "--memory=2g",
            "--memory-swap=2g",
            "--pids-limit=128",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--read-only",
            "--name",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        arguments.push(self.container(phase));
        arguments.push(
            if matches!(phase, ServerOciRunPhase::Serve) {
                "--network=host"
            } else {
                "--network=none"
            }
            .into(),
        );
        if matches!(
            phase,
            ServerOciRunPhase::RuntimeConfig | ServerOciRunPhase::Serve
        ) {
            arguments.extend([
                "--mount".into(),
                format!(
                    "type=bind,source={},target=/run/rusterisk-config,readonly",
                    self.configuration
                ),
            ]);
        }
        if matches!(phase, ServerOciRunPhase::Serve) {
            arguments.extend([
                "--tmpfs".into(),
                "/opt/rusterisk/data:rw,uid=65532,gid=65532,mode=0700,size=64m".into(),
            ]);
        }
        arguments.push(self.image.as_str().to_owned());
        match phase {
            ServerOciRunPhase::Version => arguments.push("--version".into()),
            ServerOciRunPhase::PackagedConfig => arguments.extend(
                ["--check-config", "--config-dir", "/opt/rusterisk/config"]
                    .into_iter()
                    .map(str::to_owned),
            ),
            ServerOciRunPhase::RuntimeConfig => arguments.extend(
                ["--check-config", "--config-dir", "/run/rusterisk-config"]
                    .into_iter()
                    .map(str::to_owned),
            ),
            ServerOciRunPhase::Serve => arguments.extend(
                [
                    "--config-dir",
                    "/run/rusterisk-config",
                    "--data-dir",
                    "/opt/rusterisk/data",
                ]
                .into_iter()
                .map(str::to_owned),
            ),
        }
        Step {
            arguments,
            budget: if matches!(phase, ServerOciRunPhase::Serve) {
                Duration::from_secs(90)
            } else {
                Duration::from_secs(15)
            },
        }
    }

    pub fn stop(&self) -> Step {
        Step {
            arguments: [
                "stop".into(),
                "--time=5".into(),
                self.container(ServerOciRunPhase::Serve),
            ]
            .into(),
            budget: Duration::from_secs(15),
        }
    }

    pub fn cleanup(&self, phase: ServerOciRunPhase) -> Step {
        Step {
            arguments: ["rm".into(), "--force".into(), self.container(phase)].into(),
            budget: Duration::from_secs(15),
        }
    }
}
