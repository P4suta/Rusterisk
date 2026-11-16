use crate::server_distribution::ServerTarget;

mod inputs;
pub use inputs::{InputError, InputKey, SourceInputs};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinuxOciTarget {
    X86_64,
    Arm64,
}

impl LinuxOciTarget {
    pub const fn server_target(self) -> ServerTarget {
        match self {
            Self::X86_64 => ServerTarget::LinuxX86_64,
            Self::Arm64 => ServerTarget::LinuxArm64,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequiredGate {
    Fast,
    Security,
    Reference,
    Browser,
    CoreProofs,
    CoreMiri,
    CoreMutation,
    InputFuzz,
    NativeSanitizers,
    Sdk,
    Server(ServerTarget),
    Oci(LinuxOciTarget),
}

impl RequiredGate {
    pub const ALL: [Self; 17] = [
        Self::Fast,
        Self::Security,
        Self::Reference,
        Self::Browser,
        Self::CoreProofs,
        Self::CoreMiri,
        Self::CoreMutation,
        Self::InputFuzz,
        Self::NativeSanitizers,
        Self::Sdk,
        Self::Server(ServerTarget::LinuxX86_64),
        Self::Server(ServerTarget::LinuxArm64),
        Self::Server(ServerTarget::MacOsArm64),
        Self::Server(ServerTarget::MacOsX86_64),
        Self::Server(ServerTarget::WindowsX86_64),
        Self::Oci(LinuxOciTarget::X86_64),
        Self::Oci(LinuxOciTarget::Arm64),
    ];
}
