use std::time::Duration;
mod configuration;
mod interop;
#[cfg(unix)]
mod sipp_comparison;
mod sipp_statistics;
pub use configuration::copy_configuration;
pub use interop::{
    callback_address, media_address, published_address, CallbackHost, Interop, SipTransferCase,
    SippAdapter,
};
#[cfg(unix)]
pub use sipp_comparison::{compare_sipp_registrations_with, SippRegistrationComparison};
pub use sipp_statistics::validate_sipp_statistics;

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

pub struct CoreProofBuild {
    name: String,
}

pub(crate) struct ImageIdentity(String);

impl ImageIdentity {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let digest = if let Some(digest) = value.strip_prefix("sha256:") {
            digest
        } else {
            let (name, digest) = value.rsplit_once("@sha256:")?;
            if name.is_empty()
                || name.starts_with('-')
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".-_/:".contains(&byte))
            {
                return None;
            }
            digest
        };
        if value.len() > 512
            || digest.len() != 64
            || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return None;
        }
        Some(Self(value.into()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug)]
pub enum ReferenceError {
    InvalidIdentity,
    InvalidAddress,
    InvalidConfigPath,
    IncompleteTransfer,
    IncompleteSippStatistics,
}

impl std::fmt::Display for ReferenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidIdentity => formatter.write_str("invalid reference build identity"),
            Self::InvalidAddress => formatter.write_str("invalid local reference address"),
            Self::InvalidConfigPath => formatter.write_str("invalid reference configuration path"),
            Self::IncompleteTransfer => {
                formatter.write_str("the required SIP transfer comparison did not pass")
            }
            Self::IncompleteSippStatistics => {
                formatter.write_str("SIPp statistics do not complete one successful call")
            }
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

pub(crate) fn valid_identity(identity: &str) -> bool {
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

pub(crate) fn build_steps(name: &str, tag: &str, context: &str, seconds: u64) -> Vec<Step> {
    builder_steps(name)
        .into_iter()
        .chain(std::iter::once(step(
            &[
                "buildx",
                "build",
                "--builder",
                name,
                "--load",
                "--progress=plain",
                "--tag",
                tag,
                context,
            ],
            seconds,
        )))
        .collect()
}

pub(crate) fn builder_steps(name: &str) -> [Step; 3] {
    [
        step(&["buildx", "create", "--name", name, "--driver", "docker-container", "--driver-opt", "memory=2g,memory-swap=2g,cpu-quota=200000,cpu-period=100000,restart-policy=no,image=moby/buildkit@sha256:c5f8862f52499773af85bc07695b2379ef08e8002352b0df2c4864c91d78dabe"], 30),
        step(&["buildx", "inspect", "--bootstrap", name], 180),
        step(&["update", "--pids-limit=256", &format!("buildx_buildkit_{name}0")], 30),
    ]
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

impl CoreProofBuild {
    pub fn new(identity: &str) -> Result<Self, ReferenceError> {
        if !valid_identity(identity) {
            return Err(ReferenceError::InvalidIdentity);
        }
        Ok(Self {
            name: format!("rusterisk-core-proofs-{identity}"),
        })
    }

    pub fn steps(&self) -> Vec<Step> {
        build_steps(
            &self.name,
            "rusterisk-core-proofs:kani-0.68.0",
            "containers/core-proofs",
            900,
        )
    }

    pub fn cleanup(&self) -> Step {
        step(&["buildx", "rm", "--force", &self.name], 60)
    }
}
