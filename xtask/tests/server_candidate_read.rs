#[path = "sdk_candidate_build/fixture.rs"]
mod fixture;
use fixture::{using_project, write};
use std::{io, path::PathBuf};
use xtask::{
    distribution::SdkCandidateBuildError,
    process::ProcessError,
    server_distribution::{CapturedServer, ServerCandidate, ServerTarget},
};

fn binary() -> Vec<u8> {
    let mut bytes = vec![0; 32];
    bytes[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
    bytes[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
    bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
    bytes
}

#[test]
fn server_candidates_bind_archive_checksum_source_and_executable_bytes(
) -> Result<(), SdkCandidateBuildError> {
    using_project("server-candidate-read", |root, parent| {
        write(
            root,
            "docs/server-distribution.md",
            b"Startup instructions.\n",
        )?;
        let capture =
            CapturedServer::read(root, ServerTarget::MacOsArm64).map_err(io::Error::other)?;
        let directory = parent.join("candidate");
        capture.build_with(&directory, |command, _| {
            let command = command.into_std();
            if command
                .get_args()
                .next()
                .is_some_and(|argument| argument == "build")
            {
                let target = command
                    .get_envs()
                    .find(|(name, _)| *name == "CARGO_TARGET_DIR")
                    .and_then(|(_, value)| value)
                    .ok_or(ProcessError::InvalidOutput)?;
                for path in [
                    "aarch64-apple-darwin/release/rusterisk",
                    "aarch64-apple-darwin/release/rusterisk-cli",
                ] {
                    write(&PathBuf::from(target), path, &binary())?;
                }
            }
            Ok(())
        })?;
        let candidate = ServerCandidate::read(&directory, ServerTarget::MacOsArm64)?;
        assert_eq!(candidate.name().file_name(), capture.name().file_name());
        assert_eq!(candidate.source_digest(), capture.source_digest());
        assert_eq!(candidate.files().count(), 10);
        assert!(candidate
            .files()
            .any(|(path, bytes)| path == "bin/rusterisk" && bytes == binary()));
        let extracted_root = parent.join("extracted");
        let extracted = candidate.extract(&extracted_root)?;
        for (path, bytes) in candidate.files() {
            assert_eq!(std::fs::read(extracted.directory()?.join(path))?, bytes);
        }
        assert_eq!(
            extracted.executable()?,
            extracted_root.join("bin/rusterisk")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(extracted.executable()?)?
                    .permissions()
                    .mode()
                    & 0o7777,
                0o755
            );
        }
        assert!(candidate.extract(&extracted_root).is_err());
        extracted.close()?;
        assert!(!extracted_root.exists());
        let partial = parent.join("failed-extraction");
        assert!(candidate
            .extract_with(&partial, |_, _| Err(io::Error::other(
                "injected write failure"
            )))
            .is_err());
        assert!(!partial.exists());
        let dropped = parent.join("dropped-extraction");
        drop(candidate.extract(&dropped)?);
        assert!(!dropped.exists());
        let checksum = directory.join("SHA256SUMS");
        let sums = std::fs::read(&checksum)?;
        for bytes in [b"".to_vec(), b"fake checksum\n".to_vec()] {
            std::fs::write(&checksum, bytes)?;
            assert!(ServerCandidate::read(&directory, ServerTarget::MacOsArm64).is_err());
        }
        std::fs::write(checksum, sums)?;
        let marker = directory.join("SOURCE.sha256");
        std::fs::write(marker, b"wrong source\n")?;
        assert!(ServerCandidate::read(&directory, ServerTarget::MacOsArm64).is_err());
        Ok(())
    })
}

#[test]
fn windows_candidates_check_zip_contents_even_when_the_supplied_checksum_matches(
) -> Result<(), SdkCandidateBuildError> {
    using_project("server-candidate-zip", |root, parent| {
        write(
            root,
            "docs/server-distribution.md",
            b"Startup instructions.\n",
        )?;
        let capture =
            CapturedServer::read(root, ServerTarget::WindowsX86_64).map_err(io::Error::other)?;
        let directory = parent.join("candidate");
        capture.build_with(&directory, |command, _| {
            let command = command.into_std();
            if command
                .get_args()
                .next()
                .is_some_and(|argument| argument == "build")
            {
                let target = command
                    .get_envs()
                    .find(|(name, _)| *name == "CARGO_TARGET_DIR")
                    .and_then(|(_, value)| value)
                    .ok_or(ProcessError::InvalidOutput)?;
                let mut binary = vec![0; 256];
                binary[..2].copy_from_slice(b"MZ");
                binary[60..64].copy_from_slice(&64u32.to_le_bytes());
                binary[64..68].copy_from_slice(b"PE\0\0");
                binary[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
                binary[70..72].copy_from_slice(&1u16.to_le_bytes());
                binary[84..86].copy_from_slice(&112u16.to_le_bytes());
                binary[86..88].copy_from_slice(&2u16.to_le_bytes());
                binary[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
                binary[104..108].copy_from_slice(&4096u32.to_le_bytes());
                binary[156..158].copy_from_slice(&3u16.to_le_bytes());
                for path in [
                    "x86_64-pc-windows-msvc/release/rusterisk.exe",
                    "x86_64-pc-windows-msvc/release/rusterisk-cli.exe",
                ] {
                    write(&PathBuf::from(target), path, &binary)?;
                }
            }
            Ok(())
        })?;
        let candidate = ServerCandidate::read(&directory, ServerTarget::WindowsX86_64)?;
        let name = candidate.name().file_name();
        let mut bytes = std::fs::read(directory.join(name))?;
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(&bytes)).map_err(io::Error::from)?;
        let start = archive
            .by_index(0)
            .map_err(io::Error::from)?
            .data_start()
            .ok_or_else(|| io::Error::other("fixture zip data missing"))?;
        let start = usize::try_from(start).map_err(io::Error::other)?;
        drop(archive);
        bytes[start] ^= 1;
        use sha2::{Digest, Sha256};
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        std::fs::write(directory.join(name), &bytes)?;
        std::fs::write(directory.join("SHA256SUMS"), format!("{digest}  {name}\n"))?;
        assert!(ServerCandidate::read(&directory, ServerTarget::WindowsX86_64).is_err());
        Ok(())
    })
}
