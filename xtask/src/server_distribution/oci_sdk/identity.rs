use crate::server_distribution::ServerTarget;
use serde::Deserialize;
use std::io;

pub(super) const INSPECT: &str = "{\"id\":{{json .Id}},\"os\":{{json .Os}},\"architecture\":{{json .Architecture}},\"descriptor\":{{with index . \"Descriptor\"}}{\"digest\":{{json .digest}},\"media_type\":{{json .mediaType}},\"size\":{{json .size}}}{{else}}null{{end}}}";

#[derive(Clone, Copy)]
pub(super) struct ExpectedImage<'a> {
    pub configuration: &'a str,
    pub manifest: &'a str,
    pub manifest_size: u64,
    pub target: ServerTarget,
}

pub(super) enum EngineImage<'a> {
    Configuration(ExpectedImage<'a>),
    ApplicationManifest(ExpectedImage<'a>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct View {
    id: String,
    os: String,
    architecture: String,
    descriptor: Option<Descriptor>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Descriptor {
    digest: String,
    media_type: String,
    size: u64,
}

impl<'a> EngineImage<'a> {
    pub fn loaded(bytes: &[u8], expected: ExpectedImage<'a>) -> io::Result<Self> {
        if bytes == format!("Loaded image ID: {}\n", expected.configuration).as_bytes() {
            Ok(Self::Configuration(expected))
        } else if bytes == format!("Loaded image ID: {}\n", expected.manifest).as_bytes() {
            Ok(Self::ApplicationManifest(expected))
        } else {
            Err(io::Error::other(
                "loaded OCI identity is outside the verified configuration and manifest profiles",
            ))
        }
    }

    pub fn id(&self) -> &'a str {
        match self {
            Self::Configuration(expected) => expected.configuration,
            Self::ApplicationManifest(expected) => expected.manifest,
        }
    }

    fn descriptor_matches(&self, descriptor: Option<&Descriptor>) -> bool {
        match (self, descriptor) {
            (Self::Configuration(_), None) => true,
            (Self::ApplicationManifest(expected), Some(descriptor)) => {
                descriptor.digest == expected.manifest
                    && descriptor.media_type == "application/vnd.oci.image.manifest.v1+json"
                    && descriptor.size == expected.manifest_size
            }
            (Self::Configuration(_), Some(_)) | (Self::ApplicationManifest(_), None) => false,
        }
    }

    pub fn container_matches(&self, configuration: &str, descriptor: Option<&Descriptor>) -> bool {
        let expected = match self {
            Self::Configuration(expected) | Self::ApplicationManifest(expected) => expected,
        };
        configuration == expected.configuration && self.descriptor_matches(descriptor)
    }

    pub fn check(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > 16 * 1024 {
            return Err(io::Error::other("OCI image inspection exceeds its limit"));
        }
        let view: View = serde_json::from_slice(bytes)
            .map_err(|_| io::Error::other("OCI image inspection has invalid metadata"))?;
        let expected = match self {
            Self::Configuration(expected) | Self::ApplicationManifest(expected) => expected,
        };
        let architecture = match expected.target {
            ServerTarget::LinuxArm64 => "arm64",
            ServerTarget::LinuxX86_64 => "amd64",
            ServerTarget::MacOsArm64 | ServerTarget::MacOsX86_64 | ServerTarget::WindowsX86_64 => {
                return Err(io::Error::other(
                    "OCI image inspection requires a Linux target",
                ))
            }
        };
        if view.id != self.id() || view.os != "linux" || view.architecture != architecture {
            return Err(io::Error::other("OCI image identity or platform differs"));
        }
        if self.descriptor_matches(view.descriptor.as_ref()) {
            Ok(())
        } else {
            Err(io::Error::other(
                "OCI image descriptor differs from its verified storage profile",
            ))
        }
    }
}

#[cfg(test)]
mod tests;
