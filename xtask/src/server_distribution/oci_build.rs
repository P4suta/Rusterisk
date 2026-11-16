use super::ServerOciProfile;
use crate::reference::{builder_steps, valid_identity, Step};
use std::{
    io,
    path::{Component, Path},
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerOciBuildPhase {
    CreateBuilder,
    Bootstrap,
    BoundProcesses,
    Export,
}

impl ServerOciBuildPhase {
    pub const ALL: [Self; 4] = [
        Self::CreateBuilder,
        Self::Bootstrap,
        Self::BoundProcesses,
        Self::Export,
    ];
    pub const fn label(self) -> &'static str {
        match self {
            Self::CreateBuilder => "create-builder",
            Self::Bootstrap => "bootstrap",
            Self::BoundProcesses => "bound-processes",
            Self::Export => "export",
        }
    }
}

pub struct ServerOciBuildPlan {
    name: String,
    context: String,
    output: String,
    platform: &'static str,
}

fn path(path: &Path) -> io::Result<String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err(io::Error::other(
            "OCI builds require absolute paths without traversal",
        ));
    }
    path.to_str()
        .filter(|path| {
            path.len() <= 4096 && !path.contains(',') && !path.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("OCI build paths are ambiguous or too long"))
}

impl ServerOciBuildPlan {
    pub fn new(
        identity: &str,
        context: &Path,
        output: &Path,
        profile: &ServerOciProfile,
    ) -> io::Result<Self> {
        if !valid_identity(identity) || context.starts_with(output) || output.starts_with(context) {
            return Err(io::Error::other(
                "OCI builds require an owned identity and separate paths",
            ));
        }
        Ok(Self {
            name: format!("rusterisk-server-oci-{identity}"),
            context: path(context)?,
            output: path(output)?,
            platform: profile.platform(),
        })
    }

    pub fn step(&self, phase: ServerOciBuildPhase) -> Step {
        let [create, bootstrap, bound] = builder_steps(&self.name);
        match phase {
            ServerOciBuildPhase::CreateBuilder => create,
            ServerOciBuildPhase::Bootstrap => bootstrap,
            ServerOciBuildPhase::BoundProcesses => bound,
            ServerOciBuildPhase::Export => Step {
                arguments: [
                    "buildx".to_owned(), "build".to_owned(), "--builder".to_owned(), self.name.clone(),
                    format!("--platform={}", self.platform), "--progress=plain".to_owned(), "--network=none".to_owned(),
                    "--build-arg".to_owned(), "SOURCE_DATE_EPOCH=0".to_owned(),
                    "--tag".to_owned(), format!("rusterisk-server-candidate:{}", self.name),
                    "--output".to_owned(), format!("type=oci,dest={},oci-mediatypes=true,compression=gzip,compression-level=6,rewrite-timestamp=true", self.output),
                    self.context.clone(),
                ].into(),
                budget: Duration::from_secs(300),
            },
        }
    }

    pub fn cleanup(&self) -> Step {
        Step {
            arguments: [
                "buildx".to_owned(),
                "rm".to_owned(),
                "--force".to_owned(),
                self.name.clone(),
            ]
            .into(),
            budget: Duration::from_secs(60),
        }
    }
}
