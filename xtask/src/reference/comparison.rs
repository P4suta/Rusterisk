use super::{ImageIdentity, Interop, ReferenceError, SipTransferCase, Step};
use std::time::Duration;

const IMAGE: &str = "rusterisk-asterisk-reference:22.9.0";

#[derive(Clone, Copy)]
pub enum ReferenceCase {
    Ari,
    Transfer(SipTransferCase),
    RegisteredExtensions,
    IncomingTrunk,
    OutgoingTrunk,
}

impl ReferenceCase {
    pub const ALL: [Self; 6] = [
        Self::Ari,
        Self::Transfer(SipTransferCase::Blind),
        Self::Transfer(SipTransferCase::Attended),
        Self::RegisteredExtensions,
        Self::IncomingTrunk,
        Self::OutgoingTrunk,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Ari => "resource_queries_and_app_subscriptions_match_the_reference",
            Self::Transfer(case) => case.name(),
            Self::RegisteredExtensions => "call_paths::reference_registered_extensions",
            Self::IncomingTrunk => "call_paths::reference_incoming_trunk",
            Self::OutgoingTrunk => "call_paths::reference_outgoing_trunk",
        }
    }

    pub const fn cargo_arguments(self) -> [&'static str; 11] {
        let (package, target) = match self {
            Self::Ari => ("rusterisk-client", "reference"),
            Self::Transfer(_)
            | Self::RegisteredExtensions
            | Self::IncomingTrunk
            | Self::OutgoingTrunk => ("rusterisk-server", "sip_call"),
        };
        [
            "test",
            "--locked",
            "--package",
            package,
            "--test",
            target,
            self.name(),
            "--",
            "--ignored",
            "--exact",
            "--show-output",
        ]
    }

    pub const fn budget(self) -> Duration {
        Duration::from_secs(match self {
            Self::Ari => 300,
            Self::Transfer(_)
            | Self::RegisteredExtensions
            | Self::IncomingTrunk
            | Self::OutgoingTrunk => 180,
        })
    }

    pub fn validate_execution(self, output: &str) -> Result<(), ReferenceError> {
        match self {
            Self::Ari | Self::RegisteredExtensions | Self::IncomingTrunk | Self::OutgoingTrunk => {
                validate_case(self.name(), output)
            }
            Self::Transfer(case) => case.validate_execution(output),
        }
    }
}

pub(super) fn validate_case(name: &str, output: &str) -> Result<(), ReferenceError> {
    if output.len() > 65_536 {
        return Err(ReferenceError::IncompleteCase);
    }
    let expected = format!("test {name} ... ok");
    let mut passed = false;
    let mut summarized = false;
    for line in output.lines() {
        if line.starts_with("test result:") {
            let filtered = line
                .strip_prefix("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; ")
                .and_then(|line| line.split_once("; finished in "))
                .and_then(|(filtered, _time)| filtered.strip_suffix(" filtered out"));
            if summarized
                || !filtered.is_some_and(|count| {
                    !count.is_empty()
                        && count.bytes().all(|byte| byte.is_ascii_digit())
                        && count.parse::<usize>().is_ok()
                })
            {
                return Err(ReferenceError::IncompleteCase);
            }
            summarized = true;
        } else if line.starts_with("test ") {
            if passed || line != expected {
                return Err(ReferenceError::IncompleteCase);
            }
            passed = true;
        }
    }
    if passed && summarized {
        Ok(())
    } else {
        Err(ReferenceError::IncompleteCase)
    }
}

pub struct ReferenceImage(ImageIdentity);

impl ReferenceImage {
    pub(crate) const fn new(identity: ImageIdentity) -> Self {
        Self(identity)
    }

    pub fn inspection() -> Step {
        super::step(&["image", "inspect", "--format", "{{.Id}}", IMAGE], 10)
    }

    pub fn parse_inspection(output: &str) -> Result<Self, ReferenceError> {
        let output = output.strip_suffix('\n').unwrap_or(output);
        ImageIdentity::parse(output)
            .filter(|identity| identity.as_str().starts_with("sha256:"))
            .map(Self::new)
            .ok_or(ReferenceError::InvalidIdentity)
    }

    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn start(&self, plan: &Interop) -> Result<Step, ReferenceError> {
        let mut step = plan.start();
        let mut replaced = false;
        for argument in &mut step.arguments {
            if argument == IMAGE {
                if replaced {
                    return Err(ReferenceError::InvalidIdentity);
                }
                *argument = self.as_str().into();
                replaced = true;
            }
        }
        if !replaced {
            return Err(ReferenceError::InvalidIdentity);
        }
        self.check_start(&step)?;
        Ok(step)
    }

    pub fn check_start(&self, step: &Step) -> Result<(), ReferenceError> {
        let mut entries = step
            .arguments
            .windows(3)
            .filter(|parts| parts[0] == "--entrypoint" && parts[1] == "/usr/bin/timeout");
        if step.arguments.first().map(String::as_str) != Some("run")
            || entries.next().map(|parts| parts[2].as_str()) != Some(self.as_str())
            || entries.next().is_some()
        {
            return Err(ReferenceError::InvalidIdentity);
        }
        Ok(())
    }
}
