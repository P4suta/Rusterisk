use crate::{
    distribution::{CapturedSdk, SdkContents},
    release_check::{CapturedReleaseSource, LinuxOciTarget, SourceInputs},
    server_distribution::{
        CapturedServer, ServerCandidate, ServerExecutionMode, ServerOciProfile, ServerTarget,
    },
};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

mod archive_fixture {
    use crate as xtask;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/sdk_content/fixture.rs"
    ));
}
mod captured_fixture {
    use crate as xtask;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/sdk_candidate_build/fixture.rs"
    ));
}
mod candidate_fixture {
    use crate as xtask;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/sdk_contents/fixture.rs"
    ));
}
mod oci_fixture {
    use crate as xtask;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/oci_archive/fixture.rs"
    ));
}
pub(super) use oci_fixture::Attestation;

pub(super) struct Candidates {
    pub server: PathBuf,
    pub sdk: PathBuf,
    pub target: ServerTarget,
    pub execution: PathBuf,
    pub consumer_execution: PathBuf,
    pub artifacts: PathBuf,
    archive: Option<PathBuf>,
}

pub(super) fn require(valid: bool, reason: &'static str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(reason))
    }
}

pub(super) fn native_target() -> io::Result<ServerTarget> {
    ServerTarget::ALL
        .into_iter()
        .find(|target| {
            matches!(
                ServerExecutionMode::select(
                    *target,
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    false
                ),
                Ok(ServerExecutionMode::Native)
            )
        })
        .ok_or_else(|| io::Error::other("native outcome fixtures require a supported host"))
}

pub(super) fn linux_target() -> io::Result<LinuxOciTarget> {
    match native_target()? {
        ServerTarget::LinuxArm64 | ServerTarget::MacOsArm64 => Ok(LinuxOciTarget::Arm64),
        ServerTarget::LinuxX86_64 | ServerTarget::MacOsX86_64 => Ok(LinuxOciTarget::X86_64),
        ServerTarget::WindowsX86_64 => Err(io::Error::other("OCI fixtures require a Unix host")),
    }
}

fn elf(machine: u16) -> Vec<u8> {
    let mut bytes = [0u8; 256];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&4096u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes.to_vec()
}

fn mach_o(processor: u32) -> Vec<u8> {
    let mut bytes = [0u8; 32];
    bytes[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
    bytes[4..8].copy_from_slice(&processor.to_le_bytes());
    bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
    bytes.to_vec()
}

fn pe() -> Vec<u8> {
    let mut bytes = [0u8; 256];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
    bytes[64..68].copy_from_slice(b"PE\0\0");
    bytes[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
    bytes[70..72].copy_from_slice(&1u16.to_le_bytes());
    bytes[84..86].copy_from_slice(&112u16.to_le_bytes());
    bytes[86..88].copy_from_slice(&2u16.to_le_bytes());
    bytes[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
    bytes[104..108].copy_from_slice(&4096u32.to_le_bytes());
    bytes[156..158].copy_from_slice(&3u16.to_le_bytes());
    bytes.to_vec()
}

fn binary(target: ServerTarget) -> Vec<u8> {
    match target {
        ServerTarget::LinuxX86_64 => elf(62),
        ServerTarget::LinuxArm64 => elf(183),
        ServerTarget::MacOsArm64 => mach_o(0x0100000c),
        ServerTarget::MacOsX86_64 => mach_o(0x01000007),
        ServerTarget::WindowsX86_64 => pe(),
    }
}

fn add_runtime(fixtures: &mut [archive_fixture::Fixture; 3]) -> io::Result<()> {
    let lock = fixtures[2]
        .files
        .get_mut("Cargo.lock")
        .ok_or_else(|| io::Error::other("fixture client lock missing"))?;
    lock.extend_from_slice(format!("[[package]]\nname='tokio'\nversion='1.54.1'\nsource='registry+https://github.com/rust-lang/crates.io-index'\nchecksum='{}'\n", "a".repeat(64)).as_bytes());
    Ok(())
}

fn inputs(source: &Path) -> io::Result<()> {
    for path in [
        "rust-toolchain.toml",
        "clippy.toml",
        "deny.toml",
        "justfile",
        ".gitignore",
        "crates/core/proofs/Cargo.lock",
        "crates/core/assurance/Cargo.lock",
    ] {
        captured_fixture::write(source, path, b"release fixture\n")?;
    }
    captured_fixture::write(
        source,
        "docs/server-distribution.md",
        b"Server fixture startup instructions.\n",
    )?;
    for directory in [
        "api",
        "containers",
        ".github/workflows",
        "tests/interop/node-ari-client",
        "tests/interop/sipp",
        "docs/adr",
    ] {
        fs::create_dir_all(source.join(directory))?;
    }
    for (path, bytes) in [
        (
            "config/http.conf",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../config/http.conf")).as_slice(),
        ),
        (
            "config/ari.conf",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../config/ari.conf")).as_slice(),
        ),
        (
            "config/pjsip.conf",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../config/pjsip.conf")).as_slice(),
        ),
        (
            "config/rtp.conf",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../config/rtp.conf")).as_slice(),
        ),
        (
            "config/extensions.conf",
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../config/extensions.conf"
            ))
            .as_slice(),
        ),
    ] {
        captured_fixture::write(source, path, bytes)?;
    }
    Ok(())
}

fn sdk_source(candidate: &Path) -> io::Result<()> {
    let capture = CapturedSdk::read(&candidate.join("source")).map_err(io::Error::other)?;
    fs::write(
        candidate.join("SOURCE.sha256"),
        format!("{}\n", capture.source_digest()),
    )
}

impl Candidates {
    pub fn archive(&self) -> io::Result<&Path> {
        self.archive
            .as_deref()
            .ok_or_else(|| io::Error::other("fixture has no Linux OCI archive"))
    }

    pub fn change_server_source(&self) -> io::Result<()> {
        captured_fixture::write(
            &self.server.join("source"),
            "xtask/src/lib.rs",
            b"changed server source\n",
        )?;
        let capture = CapturedServer::read(&self.server.join("source"), self.target)?;
        fs::write(
            self.server.join("SOURCE.sha256"),
            format!("{}\n", capture.source_digest()),
        )?;
        ServerCandidate::read(&self.server, self.target)?;
        Ok(())
    }

    pub fn change_sdk_source(&self) -> io::Result<()> {
        captured_fixture::write(
            &self.sdk.join("source"),
            "xtask/src/lib.rs",
            b"changed SDK source\n",
        )?;
        sdk_source(&self.sdk)?;
        SdkContents::read(&self.sdk).map_err(io::Error::other)?;
        Ok(())
    }

    fn image(&self, attestation: Attestation) -> io::Result<oci_fixture::Image> {
        let candidate = ServerCandidate::read(&self.server, self.target)?;
        let mut image = oci_fixture::Image::new(self.target)?;
        image.profile = ServerOciProfile::new(
            self.target,
            candidate.name().version(),
            candidate.source_digest(),
        )?;
        image.expected = candidate
            .files()
            .map(|(path, bytes)| (path.to_owned(), bytes.to_vec()))
            .collect();
        image
            .application
            .retain(|entry| entry.kind == tar::EntryType::Directory);
        for (path, bytes) in &image.expected {
            let mut entry = oci_fixture::Entry::file(&format!("opt/rusterisk/{path}"), bytes);
            entry.uid = 65532;
            entry.gid = 65532;
            entry.mode = if self.target.is_executable(path) {
                0o755
            } else {
                0o644
            };
            image.application.push(entry);
        }
        image.attestation = attestation;
        Ok(image)
    }

    pub fn write_oci(&self, attestation: Attestation) -> io::Result<()> {
        let image = self.image(attestation)?;
        let bytes = image.bytes()?;
        let limits = crate::server_distribution::OciArchiveLimits::SERVER.constrain(
            512 * 1024 * 1024,
            272 * 1024 * 1024,
            1024 * 1024,
            128,
            image.expanded_bytes()?,
        )?;
        crate::server_distribution::check_server_oci_archive(
            &image.profile,
            image.files(),
            io::Cursor::new(&bytes),
            limits,
        )
        .map_err(io::Error::other)?;
        fs::write(self.archive()?, bytes)?;
        let candidate = ServerCandidate::read(&self.server, self.target)?;
        candidate.check_oci(self.archive()?)?;
        Ok(())
    }

    pub fn empty_index_graph(&self) -> io::Result<Vec<u8>> {
        oci_fixture::empty_index_graph(&self.image(Attestation::None)?)
    }
}

pub(super) fn using(
    target: ServerTarget,
    test: impl FnOnce(&CapturedReleaseSource, &Candidates) -> io::Result<()>,
) -> io::Result<()> {
    candidate_fixture::using_candidate("release-runtime", add_runtime, |sdk| {
        let source = sdk.join("source");
        inputs(&source)?;
        sdk_source(sdk)?;
        let capture = CapturedServer::read(&source, target)?;
        let server = sdk.with_file_name("server");
        capture.build_with(&server, |command, _| {
            let command = command.into_std();
            if command.get_args().next().is_some_and(|arg| arg == "build") {
                let state = command
                    .get_envs()
                    .find(|(name, _)| *name == "CARGO_TARGET_DIR")
                    .and_then(|(_, value)| value)
                    .ok_or(crate::process::ProcessError::InvalidOutput)?;
                for path in [target.binary_path(), target.cli_path()] {
                    let executable = path
                        .strip_prefix("bin/")
                        .ok_or(crate::process::ProcessError::InvalidOutput)?;
                    captured_fixture::write(
                        Path::new(state),
                        &format!("{}/release/{executable}", target.triple()),
                        &binary(target),
                    )?;
                }
            }
            Ok(())
        })?;
        ServerCandidate::read(&server, target)?;
        SdkContents::read(sdk).map_err(io::Error::other)?;
        let captured = CapturedReleaseSource::read(&source).map_err(io::Error::other)?;
        let cohort = captured.inputs().map_err(io::Error::other)?;
        let sdk_capture = CapturedSdk::read(&source).map_err(io::Error::other)?;
        for projection in [
            SourceInputs::new(capture.files()).map_err(io::Error::other)?,
            SourceInputs::new(sdk_capture.files()).map_err(io::Error::other)?,
        ] {
            cohort.bind(&projection).map_err(io::Error::other)?;
        }
        let archive = matches!(target, ServerTarget::LinuxX86_64 | ServerTarget::LinuxArm64)
            .then(|| sdk.with_file_name("runtime.oci.tar"));
        let paths = Candidates {
            server,
            sdk: sdk.to_owned(),
            target,
            execution: sdk.with_file_name("server-execution"),
            consumer_execution: sdk.with_file_name("http-consumer-execution"),
            artifacts: sdk.with_file_name("runtime-artifacts"),
            archive,
        };
        if paths.archive.is_some() {
            paths.write_oci(Attestation::Artifact)?;
        }
        test(&captured, &paths)
    })
}

#[test]
fn captured_release_source_preserves_required_empty_directories_when_written() -> io::Result<()> {
    using(native_target()?, |captured, paths| {
        captured
            .write(&paths.execution, &paths.consumer_execution)
            .map_err(io::Error::other)?;
        captured.check(&paths.execution).map_err(io::Error::other)
    })
}
