use std::time::Duration;
mod interop;
pub use interop::{media_address, published_address, Interop};

pub struct Step {
    pub arguments: Vec<String>,
    pub budget: Duration,
}

pub struct ReferenceBuild {
    name: String,
}

#[derive(Debug)]
pub enum ReferenceError {
    InvalidIdentity,
    InvalidAddress,
    InvalidConfigPath,
}

impl std::fmt::Display for ReferenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidIdentity => formatter.write_str("invalid reference build identity"),
            Self::InvalidAddress => formatter.write_str("invalid local reference address"),
            Self::InvalidConfigPath => formatter.write_str("invalid reference configuration path"),
        }
    }
}

impl std::error::Error for ReferenceError {}

fn step(arguments: &[&str], seconds: u64) -> Step {
    Step {
        arguments: arguments.iter().map(|value| (*value).into()).collect(),
        budget: Duration::from_secs(seconds),
    }
}

fn valid_identity(identity: &str) -> bool {
    !identity.is_empty()
        && identity.len() <= 64
        && identity
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !identity.starts_with('-')
}

impl ReferenceBuild {
    pub fn new(identity: &str) -> Result<Self, ReferenceError> {
        if !valid_identity(identity) {
            return Err(ReferenceError::InvalidIdentity);
        }
        Ok(Self {
            name: format!("rusterisk-reference-{identity}"),
        })
    }

    pub fn steps(&self) -> Vec<Step> {
        [
            step(&["buildx", "create", "--name", &self.name, "--driver", "docker-container", "--driver-opt", "memory=2g,memory-swap=2g,cpu-quota=200000,cpu-period=100000,restart-policy=no,image=moby/buildkit@sha256:c5f8862f52499773af85bc07695b2379ef08e8002352b0df2c4864c91d78dabe"], 30),
            step(&["buildx", "inspect", "--bootstrap", &self.name], 180),
            step(&["update", "--pids-limit=256", &format!("buildx_buildkit_{}0", self.name)], 30),
            step(&["buildx", "build", "--builder", &self.name, "--load", "--progress=plain", "--tag", "rusterisk-asterisk-reference:22.9.0", "containers/asterisk-reference"], 1800),
        ].into_iter().collect()
    }

    pub fn cleanup(&self) -> Step {
        step(&["buildx", "rm", "--force", &self.name], 60)
    }
}
