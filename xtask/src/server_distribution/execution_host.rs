use super::ServerTarget;
use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerExecutionMode {
    Native,
    Rosetta,
}

impl ServerExecutionMode {
    pub fn select(
        target: ServerTarget,
        os: &str,
        architecture: &str,
        translation: bool,
    ) -> io::Result<Self> {
        let native = match target {
            ServerTarget::LinuxX86_64 => ("linux", "x86_64"),
            ServerTarget::LinuxArm64 => ("linux", "aarch64"),
            ServerTarget::MacOsArm64 => ("macos", "aarch64"),
            ServerTarget::MacOsX86_64 => ("macos", "x86_64"),
            ServerTarget::WindowsX86_64 => ("windows", "x86_64"),
        };
        if (os, architecture) == native {
            return Ok(Self::Native);
        }
        if translation
            && target == ServerTarget::MacOsX86_64
            && (os, architecture) == ("macos", "aarch64")
        {
            return Ok(Self::Rosetta);
        }
        Err(io::Error::other(
            "server candidate cannot execute under the requested host policy",
        ))
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Rosetta => "rosetta",
        }
    }
}
