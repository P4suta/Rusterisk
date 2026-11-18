use std::fmt;
mod binary_target;
pub use binary_target::{check_binary_target_header, BinaryTargetError};
mod execution_host;
pub use execution_host::ServerExecutionMode;
mod payload;
pub use payload::{PayloadError, ServerPayload};
mod output;
mod tar_gzip;
pub use tar_gzip::{check_server_tar_gzip, write_server_tar_gzip};
mod zip;
pub use zip::write_server_zip;
mod zip_read;
pub use zip_read::check_server_zip;
mod build;
pub use build::{ServerBuildPhase, ServerBuildPlan};
mod capture;
pub use capture::CapturedServer;
mod candidate;
mod command;
pub use command::{
    build_server_in_container_with, build_server_with, plan_server, plan_server_container,
};
mod read;
pub use read::ServerCandidate;
mod extraction;
pub use extraction::ExtractedServer;
mod runtime_config;
pub use runtime_config::ServerRuntimeConfig;
mod ready;
pub use ready::check_server_ready_log;
#[cfg(any(unix, windows))]
mod native;
#[cfg(any(unix, windows))]
pub(crate) mod runtime_commands;
#[cfg(any(unix, windows))]
mod windows_dependencies;
#[cfg(any(unix, windows))]
pub(crate) use native::{check_native_server_outputs, verify_loaded_server_configured};
#[cfg(any(unix, windows))]
pub use native::{
    check_server_startup_for_target_with, check_server_startup_with, verify_server_native_with,
};
#[cfg(any(unix, windows))]
mod sdk_interop;
#[cfg(any(unix, windows))]
pub use sdk_interop::verify_server_sdk_with;
mod oci_profile;
pub use oci_profile::{ServerOciProfile, OCI_RUNTIME};
mod oci_context;
pub use oci_context::ServerOciContext;
mod oci_build;
pub use oci_build::{ServerOciBuildPhase, ServerOciBuildPlan};
mod oci_export;
pub use oci_export::{export_server_candidate_oci_with, export_server_oci_with};
mod oci_runtime;
pub use oci_runtime::{ServerOciRunPhase, ServerOciRuntimePlan};
mod oci_archive;
#[cfg(unix)]
mod oci_sdk;
pub use oci_archive::{
    check_server_oci_archive, OciArchiveError, OciArchiveLimit, OciArchiveLimits, ServerOciContents,
};
#[cfg(unix)]
pub use oci_sdk::verify_server_oci_sdk_with;
#[cfg(unix)]
pub(crate) use oci_sdk::{
    check_oci_runtime_outputs, copy_archive, verify_loaded_oci_sdk_configured, OciEngineProfile,
    OciNativeContext, OciRuntimeInputs, OciSdkExecution,
};
mod container_build;
pub use container_build::{ServerBuildCommandMode, ServerContainerBuildPlan};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveFormat {
    TarGzip,
    Zip,
}

impl ArchiveFormat {
    pub const fn extension(self) -> &'static str {
        match self {
            Self::TarGzip => "tar.gz",
            Self::Zip => "zip",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerTarget {
    LinuxX86_64,
    LinuxArm64,
    MacOsArm64,
    MacOsX86_64,
    WindowsX86_64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateNameError {
    Target,
    Version,
}

impl fmt::Display for CandidateNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Target => "unsupported server distribution target",
            Self::Version => "server candidate requires a bounded portable semantic version",
        })
    }
}

impl std::error::Error for CandidateNameError {}

impl ServerTarget {
    pub const ALL: [Self; 5] = [
        Self::LinuxX86_64,
        Self::LinuxArm64,
        Self::MacOsArm64,
        Self::MacOsX86_64,
        Self::WindowsX86_64,
    ];

    pub fn parse(triple: &str) -> Result<Self, CandidateNameError> {
        Self::ALL
            .into_iter()
            .find(|target| target.triple() == triple)
            .ok_or(CandidateNameError::Target)
    }

    pub const fn triple(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "x86_64-unknown-linux-gnu",
            Self::LinuxArm64 => "aarch64-unknown-linux-gnu",
            Self::MacOsArm64 => "aarch64-apple-darwin",
            Self::MacOsX86_64 => "x86_64-apple-darwin",
            Self::WindowsX86_64 => "x86_64-pc-windows-msvc",
        }
    }

    pub const fn format(self) -> ArchiveFormat {
        match self {
            Self::LinuxX86_64 | Self::LinuxArm64 | Self::MacOsArm64 | Self::MacOsX86_64 => {
                ArchiveFormat::TarGzip
            }
            Self::WindowsX86_64 => ArchiveFormat::Zip,
        }
    }

    pub const fn binary_path(self) -> &'static str {
        match self {
            Self::WindowsX86_64 => "bin/rusterisk.exe",
            Self::LinuxX86_64 | Self::LinuxArm64 | Self::MacOsArm64 | Self::MacOsX86_64 => {
                "bin/rusterisk"
            }
        }
    }

    pub const fn required_files(self) -> [&'static str; 9] {
        [
            self.binary_path(),
            "README.md",
            "LICENSE-MIT",
            "LICENSE-APACHE",
            "config/http.conf",
            "config/ari.conf",
            "config/pjsip.conf",
            "config/rtp.conf",
            "config/extensions.conf",
        ]
    }
}

pub struct ServerCandidateName {
    version: String,
    target: ServerTarget,
    root: String,
    file_name: String,
}

impl ServerCandidateName {
    pub fn new(version: &str, target: ServerTarget) -> Result<Self, CandidateNameError> {
        if version.len() > 64 || semver::Version::parse(version).is_err() {
            return Err(CandidateNameError::Version);
        }
        let root = format!("rusterisk-{version}-{}", target.triple());
        crate::regular_archive::ArchiveRoot::new(&root).map_err(|_| CandidateNameError::Version)?;
        let file_name = format!("{root}.{}", target.format().extension());
        Ok(Self {
            version: version.to_owned(),
            target,
            root,
            file_name,
        })
    }
    pub fn root(&self) -> &str {
        &self.root
    }
    pub fn file_name(&self) -> &str {
        &self.file_name
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub const fn target(&self) -> ServerTarget {
        self.target
    }
}
