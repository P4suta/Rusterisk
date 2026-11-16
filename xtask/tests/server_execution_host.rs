use std::io;
use xtask::server_distribution::{ServerExecutionMode, ServerTarget};

#[test]
fn execution_modes_require_matching_platforms_and_explicit_translation() -> io::Result<()> {
    for (target, os, architecture) in [
        (ServerTarget::LinuxX86_64, "linux", "x86_64"),
        (ServerTarget::LinuxArm64, "linux", "aarch64"),
        (ServerTarget::MacOsArm64, "macos", "aarch64"),
        (ServerTarget::MacOsX86_64, "macos", "x86_64"),
        (ServerTarget::WindowsX86_64, "windows", "x86_64"),
    ] {
        assert_eq!(
            ServerExecutionMode::select(target, os, architecture, false)?,
            ServerExecutionMode::Native
        );
        assert_eq!(
            ServerExecutionMode::select(target, os, architecture, true)?,
            ServerExecutionMode::Native
        );
    }
    assert!(
        ServerExecutionMode::select(ServerTarget::MacOsX86_64, "macos", "aarch64", false).is_err()
    );
    assert_eq!(
        ServerExecutionMode::select(ServerTarget::MacOsX86_64, "macos", "aarch64", true)?,
        ServerExecutionMode::Rosetta
    );
    for target in ServerTarget::ALL {
        for (os, architecture) in [
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("macos", "aarch64"),
            ("macos", "x86_64"),
            ("windows", "x86_64"),
            ("unsupported", "x86_64"),
        ] {
            let native = matches!(
                (target, os, architecture),
                (ServerTarget::LinuxX86_64, "linux", "x86_64")
                    | (ServerTarget::LinuxArm64, "linux", "aarch64")
                    | (ServerTarget::MacOsArm64, "macos", "aarch64")
                    | (ServerTarget::MacOsX86_64, "macos", "x86_64")
                    | (ServerTarget::WindowsX86_64, "windows", "x86_64")
            );
            let translated =
                target == ServerTarget::MacOsX86_64 && os == "macos" && architecture == "aarch64";
            assert_eq!(
                ServerExecutionMode::select(target, os, architecture, true).is_ok(),
                native || translated
            );
        }
    }
    Ok(())
}
