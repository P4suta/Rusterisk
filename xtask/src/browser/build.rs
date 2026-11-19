use crate::reference::{build_steps, valid_identity, ReferenceError, Step};
use std::time::Duration;

pub struct Build {
    name: String,
}

impl Build {
    pub fn new(identity: &str) -> Result<Self, ReferenceError> {
        if !valid_identity(identity) {
            return Err(ReferenceError::InvalidIdentity);
        }
        Ok(Self {
            name: format!("rusterisk-browser-{identity}"),
        })
    }

    pub fn steps(&self) -> Vec<Step> {
        build_steps(
            &self.name,
            "rusterisk-browser-verification:rust-1.100.0-playwright-1.65.1",
            "containers/browser-verification",
            900,
        )
    }

    pub fn cleanup(&self) -> Step {
        Step {
            arguments: ["buildx", "rm", "--force", &self.name]
                .into_iter()
                .map(String::from)
                .collect(),
            budget: Duration::from_secs(60),
        }
    }
}
