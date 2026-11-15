use std::time::Duration;
mod configuration;
mod interop;
pub use configuration::copy_configuration;
pub use interop::{callback_address, media_address, published_address, CallbackHost, Interop};

pub struct Step {
    pub arguments: Vec<String>,
    pub budget: Duration,
}

pub struct ReferenceBuild {
    name: String,
}

pub struct VerificationBuild {
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
        build_steps(
            &self.name,
            "rusterisk-asterisk-reference:22.9.0",
            "containers/asterisk-reference",
            1800,
        )
    }

    pub fn cleanup(&self) -> Step {
        step(&["buildx", "rm", "--force", &self.name], 60)
    }
}

fn build_steps(name: &str, tag: &str, context: &str, seconds: u64) -> Vec<Step> {
    [
        step(&["buildx", "create", "--name", name, "--driver", "docker-container", "--driver-opt", "memory=2g,memory-swap=2g,cpu-quota=200000,cpu-period=100000,restart-policy=no,image=moby/buildkit@sha256:c5f8862f52499773af85bc07695b2379ef08e8002352b0df2c4864c91d78dabe"], 30),
        step(&["buildx", "inspect", "--bootstrap", name], 180),
        step(&["update", "--pids-limit=256", &format!("buildx_buildkit_{name}0")], 30),
        step(&["buildx", "build", "--builder", name, "--load", "--progress=plain", "--tag", tag, context], seconds),
    ].into_iter().collect()
}

impl VerificationBuild {
    pub fn new(identity: &str) -> Result<Self, ReferenceError> {
        if !valid_identity(identity) {
            return Err(ReferenceError::InvalidIdentity);
        }
        Ok(Self {
            name: format!("rusterisk-verification-{identity}"),
        })
    }

    pub fn steps(&self) -> Vec<Step> {
        build_steps(
            &self.name,
            "rusterisk-input-verification:rust-1.99.0",
            "containers/input-verification",
            900,
        )
    }

    pub fn cleanup(&self) -> Step {
        step(&["buildx", "rm", "--force", &self.name], 60)
    }
}
