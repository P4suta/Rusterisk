use super::{ServerCandidateName, ServerTarget};
use std::io;

pub const OCI_RUNTIME: &str =
    "debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2";

pub struct ServerOciProfile {
    target: LinuxTarget,
    version: String,
    source: String,
}

enum LinuxTarget {
    X86_64,
    Arm64,
}

impl ServerOciProfile {
    pub fn new(target: ServerTarget, version: &str, source: &str) -> io::Result<Self> {
        let linux = match target {
            ServerTarget::LinuxX86_64 => LinuxTarget::X86_64,
            ServerTarget::LinuxArm64 => LinuxTarget::Arm64,
            ServerTarget::MacOsArm64 | ServerTarget::MacOsX86_64 | ServerTarget::WindowsX86_64 => {
                return Err(io::Error::other(
                    "OCI server profiles require Linux candidates",
                ));
            }
        };
        if source.len() != 64
            || !source
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(io::Error::other(
                "OCI server profiles require a source SHA-256 digest",
            ));
        }
        let name = ServerCandidateName::new(version, target).map_err(io::Error::other)?;
        Ok(Self {
            target: linux,
            version: name.version().to_owned(),
            source: source.to_owned(),
        })
    }

    pub const fn target(&self) -> ServerTarget {
        match self.target {
            LinuxTarget::X86_64 => ServerTarget::LinuxX86_64,
            LinuxTarget::Arm64 => ServerTarget::LinuxArm64,
        }
    }
    pub fn source_digest(&self) -> &str {
        &self.source
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub const fn platform(&self) -> &'static str {
        match self.target {
            LinuxTarget::X86_64 => "linux/amd64",
            LinuxTarget::Arm64 => "linux/arm64",
        }
    }

    pub fn dockerfile(&self) -> String {
        format!(
            "FROM {OCI_RUNTIME}\n\
             COPY --chown=65532:65532 payload/ /opt/rusterisk/\n\
             LABEL org.opencontainers.image.source=\"https://github.com/P4suta/Rusterisk\" \\\n+                 org.opencontainers.image.licenses=\"MIT OR Apache-2.0\" \\\n+                 org.opencontainers.image.version=\"{}\" \\\n+                 io.rusterisk.source.sha256=\"{}\"\n\
             WORKDIR /opt/rusterisk\n\
             USER 65532:65532\n\
             STOPSIGNAL SIGTERM\n\
             EXPOSE 8088/tcp 5060/udp\n\
             ENTRYPOINT [\"/opt/rusterisk/bin/rusterisk\"]\n\
             CMD [\"--config-dir\",\"/opt/rusterisk/config\",\"--data-dir\",\"/opt/rusterisk/data\"]\n",
            self.version(), self.source_digest(),
        )
    }
}
