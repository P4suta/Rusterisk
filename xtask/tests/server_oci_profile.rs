use std::io;
use xtask::server_distribution::{ServerOciProfile, ServerTarget};

#[test]
fn runtime_profiles_require_linux_candidates_and_bounded_source_identity() -> io::Result<()> {
    let source = "a".repeat(64);
    for target in ServerTarget::ALL {
        let result = ServerOciProfile::new(target, "0.1.0", &source);
        match target {
            ServerTarget::LinuxX86_64 | ServerTarget::LinuxArm64 => {
                let profile = result?;
                assert_eq!(profile.target(), target);
                assert_eq!(profile.source_digest(), source);
                assert_eq!(profile.version(), "0.1.0");
            }
            ServerTarget::MacOsArm64 | ServerTarget::MacOsX86_64 | ServerTarget::WindowsX86_64 => {
                assert!(result.is_err());
            }
        }
    }
    for source in ["", "a", &"A".repeat(64), &"g".repeat(64), &"a".repeat(65)] {
        assert!(ServerOciProfile::new(ServerTarget::LinuxArm64, "0.1.0", source).is_err());
    }
    for version in ["", "latest", "0.1.0\nUSER 0", "0.1.0+nonportable"] {
        assert!(ServerOciProfile::new(ServerTarget::LinuxArm64, version, &source).is_err());
    }
    Ok(())
}

#[test]
fn runtime_profiles_copy_payloads_into_an_unprivileged_signal_receiving_process() -> io::Result<()>
{
    for (target, platform) in [
        (ServerTarget::LinuxX86_64, "linux/amd64"),
        (ServerTarget::LinuxArm64, "linux/arm64"),
    ] {
        let profile = ServerOciProfile::new(target, "0.1.0", &"a".repeat(64))?;
        assert_eq!(profile.platform(), platform);
        let source = profile.dockerfile();
        assert!(source.starts_with("FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2\n"));
        for required in [
            "COPY --chown=65532:65532 payload/ /opt/rusterisk/\n",
            "USER 65532:65532\n",
            "STOPSIGNAL SIGTERM\n",
            "ENTRYPOINT [\"/opt/rusterisk/bin/rusterisk\"]\n",
            "CMD [\"--config-dir\",\"/opt/rusterisk/config\",\"--data-dir\",\"/opt/rusterisk/data\"]\n",
            "org.opencontainers.image.licenses=\"MIT OR Apache-2.0\"",
            "org.opencontainers.image.version=\"0.1.0\"",
        ] {
            assert!(source.contains(required), "{required}");
        }
        assert!(!source.contains("RUN "));
        assert!(!source.contains("cargo "));
        assert!(source
            .lines()
            .all(|line| !line.trim_start().starts_with('+')));
    }
    Ok(())
}
