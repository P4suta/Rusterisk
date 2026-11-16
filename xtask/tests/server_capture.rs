#[path = "sdk_candidate_build/fixture.rs"]
mod fixture;
use fixture::{using_project, write};
use std::{io, path::Path};
use xtask::{
    distribution::SdkCandidateBuildError,
    server_distribution::{CapturedServer, ServerTarget},
};

#[test]
fn server_capture_owns_build_inputs_configuration_and_distribution_readme(
) -> Result<(), SdkCandidateBuildError> {
    using_project("server-capture", |root, parent| {
        write(
            root,
            "docs/server-distribution.md",
            b"Captured startup instructions.\n",
        )?;
        let capture =
            CapturedServer::read(root, ServerTarget::MacOsArm64).map_err(io::Error::other)?;
        assert_eq!(capture.name().version(), "0.1.0");
        assert_eq!(capture.name().target(), ServerTarget::MacOsArm64);
        assert_eq!(capture.source_digest().len(), 64);
        write(root, "config/http.conf", b"changed live configuration")?;
        write(
            root,
            "docs/server-distribution.md",
            b"Changed live instructions.\n",
        )?;
        let source = parent.join("source");
        let state = parent.join("state");
        capture.write(&source, &state).map_err(io::Error::other)?;
        assert_eq!(std::fs::read(source.join("config/http.conf"))?, b"fixture");
        assert_eq!(
            std::fs::read(source.join("docs/server-distribution.md"))?,
            b"Captured startup instructions.\n"
        );
        capture.check(&source).map_err(io::Error::other)?;
        std::fs::write(source.join("docs/server-distribution.md"), b"tampered")?;
        assert!(capture.check(&source).is_err());
        assert!(capture.write(&source, &state).is_err());
        assert!(capture
            .files()
            .any(|(path, bytes)| path == Path::new("config/http.conf") && bytes == b"fixture"));
        Ok(())
    })
}

#[test]
fn capture_identity_changes_with_readme_and_refuses_missing_or_oversize_text(
) -> Result<(), SdkCandidateBuildError> {
    using_project("server-capture-inputs", |root, _| {
        assert!(CapturedServer::read(root, ServerTarget::MacOsArm64).is_err());
        write(root, "docs/server-distribution.md", b"first")?;
        let first = CapturedServer::read(root, ServerTarget::MacOsArm64)
            .map_err(io::Error::other)?
            .source_digest();
        for bytes in [
            b"".to_vec(),
            vec![b'x'; 65_537],
            [0xff].to_vec(),
            b"nul\0".to_vec(),
        ] {
            write(root, "docs/server-distribution.md", &bytes)?;
            assert!(CapturedServer::read(root, ServerTarget::MacOsArm64).is_err());
        }
        write(root, "docs/server-distribution.md", b"second")?;
        let second = CapturedServer::read(root, ServerTarget::MacOsArm64)
            .map_err(io::Error::other)?
            .source_digest();
        assert_ne!(first, second);
        write(root, "Cargo.toml", b"[workspace]\nmembers=[]\n")?;
        assert!(CapturedServer::read(root, ServerTarget::MacOsArm64).is_err());
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn server_startup_instructions_cannot_redirect_to_another_file(
) -> Result<(), SdkCandidateBuildError> {
    using_project("server-capture-link", |root, parent| {
        write(parent, "external.md", b"external")?;
        std::os::unix::fs::symlink(
            parent.join("external.md"),
            root.join("docs/server-distribution.md"),
        )?;
        assert!(CapturedServer::read(root, ServerTarget::MacOsArm64).is_err());
        Ok(())
    })
}
