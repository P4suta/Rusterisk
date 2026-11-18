use super::{CandidateCheckError, CandidateImages, CandidateProfile, LinuxOciTarget, RequiredGate};
use crate::{
    input_fuzz::LinuxTarget,
    reference::ReferenceImage,
    server_distribution::{ServerExecutionMode, ServerTarget},
};
use serde::{Deserialize, Deserializer};
use std::{
    fmt,
    path::{Path, PathBuf},
};

const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_PATH_BYTES: usize = 4096;

#[cfg(all(test, windows))]
mod windows_path_tests;

#[derive(Debug)]
pub enum ReleaseInputError {
    Arguments,
    Gate,
    Limit,
    Encoding,
    Syntax(String),
    Format(u32),
    Path,
    Architecture,
    Profile(CandidateCheckError),
    Execution(ServerTarget),
}

impl fmt::Display for ReleaseInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arguments => formatter.write_str(
                "release-check requires explicit runner inputs or --dry-run",
            ),
            Self::Gate => formatter.write_str("release-check gate is not in the required inventory"),
            Self::Limit => formatter.write_str("release inputs exceed the 64 KiB byte limit"),
            Self::Encoding => formatter.write_str("release inputs must be UTF-8"),
            Self::Syntax(diagnostic) => formatter.write_str(diagnostic),
            Self::Format(format) => write!(formatter, "unsupported release input format: {format}"),
            Self::Path => formatter.write_str(
                "release input paths must be nonempty UTF-8 without control characters and at most 4096 bytes",
            ),
            Self::Architecture => formatter.write_str(
                "release fuzz architecture must be a supported Linux target triple",
            ),
            Self::Profile(error) => error.fmt(formatter),
            Self::Execution(target) => write!(
                formatter,
                "Rosetta requires the x86_64-apple-darwin server target: received {}",
                target.triple()
            ),
        }
    }
}

impl std::error::Error for ReleaseInputError {}

fn path_value(value: &str) -> Result<(), ReleaseInputError> {
    if value.is_empty() || value.len() > MAX_PATH_BYTES || value.chars().any(char::is_control) {
        Err(ReleaseInputError::Path)
    } else {
        Ok(())
    }
}

fn request_path(value: &str) -> Result<PathBuf, ReleaseInputError> {
    path_value(value)?;
    Ok(PathBuf::from(value))
}

pub enum ReleaseRequest {
    Inventory,
    Run { gate: RequiredGate, inputs: PathBuf },
    Verify { inputs: PathBuf },
}

impl ReleaseRequest {
    /// Parses a release operation without reading inputs or running a gate.
    pub fn parse(arguments: &[&str]) -> Result<Self, ReleaseInputError> {
        match arguments {
            ["--dry-run"] => Ok(Self::Inventory),
            ["run", name, "--inputs", inputs] => {
                let gate = RequiredGate::ALL
                    .into_iter()
                    .find(|gate| gate.to_string() == *name)
                    .ok_or(ReleaseInputError::Gate)?;
                Ok(Self::Run {
                    gate,
                    inputs: request_path(inputs)?,
                })
            }
            ["verify", "--inputs", inputs] => Ok(Self::Verify {
                inputs: request_path(inputs)?,
            }),
            _ => Err(ReleaseInputError::Arguments),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Images {
    reference: String,
    browser: String,
    #[serde(rename = "core-proofs")]
    core_proofs: String,
    #[serde(rename = "core-miri")]
    core_miri: String,
    #[serde(rename = "core-mutation")]
    core_mutation: String,
    #[serde(rename = "input-fuzz")]
    input_fuzz: String,
    #[serde(rename = "native-sanitizers")]
    native_sanitizers: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SdkPaths {
    candidate: PathBuf,
    artifact: PathBuf,
}

fn execution<'de, D: Deserializer<'de>>(deserializer: D) -> Result<ServerExecutionMode, D::Error> {
    match String::deserialize(deserializer)?.as_str() {
        "native" => Ok(ServerExecutionMode::Native),
        "rosetta" => Ok(ServerExecutionMode::Rosetta),
        _ => Err(serde::de::Error::custom(
            "server execution must be native or rosetta",
        )),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerPaths {
    candidate: PathBuf,
    artifact: PathBuf,
    #[serde(deserialize_with = "execution")]
    execution: ServerExecutionMode,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Servers {
    #[serde(rename = "x86_64-unknown-linux-gnu")]
    linux_x86_64: ServerPaths,
    #[serde(rename = "aarch64-unknown-linux-gnu")]
    linux_arm64: ServerPaths,
    #[serde(rename = "aarch64-apple-darwin")]
    macos_arm64: ServerPaths,
    #[serde(rename = "x86_64-apple-darwin")]
    macos_x86_64: ServerPaths,
    #[serde(rename = "x86_64-pc-windows-msvc")]
    windows_x86_64: ServerPaths,
}

impl Servers {
    const fn get(&self, target: ServerTarget) -> &ServerPaths {
        match target {
            ServerTarget::LinuxX86_64 => &self.linux_x86_64,
            ServerTarget::LinuxArm64 => &self.linux_arm64,
            ServerTarget::MacOsArm64 => &self.macos_arm64,
            ServerTarget::MacOsX86_64 => &self.macos_x86_64,
            ServerTarget::WindowsX86_64 => &self.windows_x86_64,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OciPaths {
    archive: PathBuf,
    artifact: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Oci {
    #[serde(rename = "x86_64-unknown-linux-gnu")]
    x86_64: OciPaths,
    #[serde(rename = "aarch64-unknown-linux-gnu")]
    arm64: OciPaths,
}

impl Oci {
    const fn get(&self, target: LinuxOciTarget) -> &OciPaths {
        match target {
            LinuxOciTarget::X86_64 => &self.x86_64,
            LinuxOciTarget::Arm64 => &self.arm64,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Checks {
    reference: PathBuf,
    browser: PathBuf,
    #[serde(rename = "core-proofs")]
    core_proofs: PathBuf,
    #[serde(rename = "core-miri")]
    core_miri: PathBuf,
    #[serde(rename = "core-mutation")]
    core_mutation: PathBuf,
    #[serde(rename = "input-fuzz")]
    input_fuzz: PathBuf,
    #[serde(rename = "native-sanitizers")]
    native_sanitizers: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: u32,
    source: PathBuf,
    fuzz_architecture: String,
    images: Images,
    sdk: SdkPaths,
    servers: Servers,
    oci: Oci,
    checks: Checks,
}

fn resolve(path: &mut PathBuf, base: &Path) -> Result<(), ReleaseInputError> {
    let value = path.to_str().ok_or(ReleaseInputError::Path)?;
    path_value(value)?;
    #[cfg(windows)]
    if value.strip_prefix(r"\\?\").is_some_and(|suffix| {
        matches!(suffix.as_bytes(), [drive, b':', rest @ ..]
            if drive.is_ascii_alphabetic() && !rest.starts_with(b"\\"))
    }) {
        return Err(ReleaseInputError::Path);
    }
    if path.is_relative() {
        #[cfg(windows)]
        if path.has_root()
            || matches!(
                path.components().next(),
                Some(std::path::Component::Prefix(_))
            )
        {
            return Err(ReleaseInputError::Path);
        }
        *path = base.join(&*path);
    }
    Ok(())
}

impl Document {
    fn resolve(&mut self, base: &Path) -> Result<(), ReleaseInputError> {
        for path in [
            &mut self.source,
            &mut self.sdk.candidate,
            &mut self.sdk.artifact,
            &mut self.servers.linux_x86_64.candidate,
            &mut self.servers.linux_x86_64.artifact,
            &mut self.servers.linux_arm64.candidate,
            &mut self.servers.linux_arm64.artifact,
            &mut self.servers.macos_arm64.candidate,
            &mut self.servers.macos_arm64.artifact,
            &mut self.servers.macos_x86_64.candidate,
            &mut self.servers.macos_x86_64.artifact,
            &mut self.servers.windows_x86_64.candidate,
            &mut self.servers.windows_x86_64.artifact,
            &mut self.oci.x86_64.archive,
            &mut self.oci.x86_64.artifact,
            &mut self.oci.arm64.archive,
            &mut self.oci.arm64.artifact,
            &mut self.checks.reference,
            &mut self.checks.browser,
            &mut self.checks.core_proofs,
            &mut self.checks.core_miri,
            &mut self.checks.core_mutation,
            &mut self.checks.input_fuzz,
            &mut self.checks.native_sanitizers,
        ] {
            resolve(path, base)?;
        }
        Ok(())
    }
}

pub struct ReleaseInputs {
    source: PathBuf,
    profile: CandidateProfile,
    reference_image: ReferenceImage,
    sdk: SdkPaths,
    servers: Servers,
    oci: Oci,
    checks: Checks,
}

impl ReleaseInputs {
    /// Parses bounded input bytes and resolves relative paths from the input file's parent.
    pub fn parse(bytes: &[u8], input_file: &Path) -> Result<Self, ReleaseInputError> {
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(ReleaseInputError::Limit);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| ReleaseInputError::Encoding)?;
        let mut document: Document = toml::from_str(text)
            .map_err(|error: toml::de::Error| ReleaseInputError::Syntax(error.to_string()))?;
        if document.format != 1 {
            return Err(ReleaseInputError::Format(document.format));
        }
        let architecture = [LinuxTarget::Arm64, LinuxTarget::Amd64]
            .into_iter()
            .find(|target| target.triple() == document.fuzz_architecture)
            .ok_or(ReleaseInputError::Architecture)?;
        let reference_image = ReferenceImage::parse_inspection(&document.images.reference)
            .map_err(|_| {
                ReleaseInputError::Profile(CandidateCheckError::Profile(RequiredGate::Reference))
            })?;
        if reference_image.as_str() != document.images.reference {
            return Err(ReleaseInputError::Profile(CandidateCheckError::Profile(
                RequiredGate::Reference,
            )));
        }
        let profile = CandidateProfile::new(
            CandidateImages {
                reference: &reference_image,
                browser: &document.images.browser,
                core_proofs: &document.images.core_proofs,
                core_miri: &document.images.core_miri,
                core_mutation: &document.images.core_mutation,
                input_fuzz: &document.images.input_fuzz,
                native_sanitizers: &document.images.native_sanitizers,
            },
            architecture,
        )
        .map_err(ReleaseInputError::Profile)?;
        for target in ServerTarget::ALL {
            if document.servers.get(target).execution == ServerExecutionMode::Rosetta
                && target != ServerTarget::MacOsX86_64
            {
                return Err(ReleaseInputError::Execution(target));
            }
        }
        let base = input_file.parent().ok_or(ReleaseInputError::Path)?;
        document.resolve(base)?;
        Ok(Self {
            source: document.source,
            profile,
            reference_image,
            sdk: document.sdk,
            servers: document.servers,
            oci: document.oci,
            checks: document.checks,
        })
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    pub const fn profile(&self) -> &CandidateProfile {
        &self.profile
    }

    pub const fn reference_image(&self) -> &ReferenceImage {
        &self.reference_image
    }

    pub fn sdk_candidate(&self) -> &Path {
        &self.sdk.candidate
    }

    pub fn server_candidate(&self, target: ServerTarget) -> &Path {
        &self.servers.get(target).candidate
    }

    pub const fn server_execution(&self, target: ServerTarget) -> ServerExecutionMode {
        self.servers.get(target).execution
    }

    pub fn oci_archive(&self, target: LinuxOciTarget) -> &Path {
        &self.oci.get(target).archive
    }

    pub fn artifact(&self, gate: RequiredGate) -> Option<&Path> {
        match gate {
            RequiredGate::Fast | RequiredGate::Security => None,
            RequiredGate::Reference => Some(&self.checks.reference),
            RequiredGate::Browser => Some(&self.checks.browser),
            RequiredGate::CoreProofs => Some(&self.checks.core_proofs),
            RequiredGate::CoreMiri => Some(&self.checks.core_miri),
            RequiredGate::CoreMutation => Some(&self.checks.core_mutation),
            RequiredGate::InputFuzz => Some(&self.checks.input_fuzz),
            RequiredGate::NativeSanitizers => Some(&self.checks.native_sanitizers),
            RequiredGate::Sdk => Some(&self.sdk.artifact),
            RequiredGate::Server(target) => Some(&self.servers.get(target).artifact),
            RequiredGate::Oci(target) => Some(&self.oci.get(target).artifact),
        }
    }
}
