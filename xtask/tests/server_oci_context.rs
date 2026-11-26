#[path = "sdk_candidate_build/fixture.rs"]
mod fixture;
use fixture::{using_project, write};
use std::{io, path::PathBuf};
use xtask::{
    distribution::SdkCandidateBuildError,
    process::ProcessError,
    server_distribution::{CapturedServer, ServerCandidate, ServerOciContext, ServerTarget},
};

fn binary() -> Vec<u8> {
    let mut bytes = vec![0; 64];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&183_u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&4096_u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
    bytes
}

#[test]
fn image_contexts_copy_only_verified_candidate_bytes_and_remove_owned_files(
) -> Result<(), SdkCandidateBuildError> {
    using_project("server-oci-context", |root, parent| {
        write(
            root,
            "docs/server-distribution.md",
            b"Startup instructions.\n",
        )?;
        let capture =
            CapturedServer::read(root, ServerTarget::LinuxArm64).map_err(io::Error::other)?;
        let directory = parent.join("candidate");
        capture.build_with(&directory, |command, _| {
            let command = command.into_std();
            if command
                .get_args()
                .next()
                .is_some_and(|argument| argument == "build")
            {
                let state = command
                    .get_envs()
                    .find(|(name, _)| *name == "CARGO_TARGET_DIR")
                    .and_then(|(_, value)| value)
                    .ok_or(ProcessError::InvalidOutput)?;
                for path in [
                    "aarch64-unknown-linux-gnu/release/rusterisk",
                    "aarch64-unknown-linux-gnu/release/rusterisk-cli",
                ] {
                    write(&PathBuf::from(state), path, &binary())?;
                }
            }
            Ok(())
        })?;
        let candidate = ServerCandidate::read(&directory, ServerTarget::LinuxArm64)?;
        let physical = std::fs::canonicalize(parent)?;
        let destination = physical.join("image-context");
        let context = ServerOciContext::create(&candidate, &destination)?;
        context.check()?;
        assert_eq!(context.directory()?, destination);
        assert_eq!(context.profile().source_digest(), candidate.source_digest());
        for (path, bytes) in candidate.files() {
            assert_eq!(
                std::fs::read(destination.join("payload").join(path))?,
                bytes
            );
        }
        assert!(destination.join("payload/data").is_dir());
        assert_eq!(
            std::fs::read_dir(destination.join("payload/data"))?.count(),
            0
        );
        assert!(ServerOciContext::create(&candidate, &destination).is_err());
        for (relative, changed) in [
            ("Dockerfile", b"FROM unpinned\n".as_slice()),
            ("payload/config/http.conf", b"changed".as_slice()),
        ] {
            let path = destination.join(relative);
            let original = std::fs::read(&path)?;
            std::fs::write(&path, changed)?;
            assert!(context.check().is_err());
            std::fs::write(path, original)?;
        }
        std::fs::write(destination.join("payload/unexpected"), b"extra")?;
        assert!(context.check().is_err());
        std::fs::remove_file(destination.join("payload/unexpected"))?;
        context.check()?;
        context.close()?;
        assert!(!destination.exists());
        let dropped = physical.join("dropped-context");
        drop(ServerOciContext::create(&candidate, &dropped)?);
        assert!(!dropped.exists());
        assert!(directory.is_dir());
        Ok(())
    })
}
