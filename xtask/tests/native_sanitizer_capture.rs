#[path = "sdk_candidate_build/fixture.rs"]
mod fixture;
use xtask::native_sanitizers::{CapturedNative, CommandMode};
use xtask::process::ProcessError;

const IMAGE: &str = "sha256:8e26e6eeb29b66c3849295c5606adaed78a2c6bd9ef17644370256c7fa977d20";

#[test]
fn native_capture_uses_owned_input_preserves_failure_logs_and_cleans_execution_state(
) -> Result<(), xtask::distribution::SdkCandidateBuildError> {
    fixture::using_project("native-capture", |root, parent| {
        let capture = CapturedNative::read(root).map_err(std::io::Error::other)?;
        fixture::write(root, "crates/core/src/lib.rs", b"changed after capture")?;
        let destination = parent.join("native-result");
        let mut executed = None;
        let mut calls = 0;
        let result = capture.run_with(&destination, IMAGE, |command, _, mode| {
            calls += 1;
            assert_eq!(mode, CommandMode::Work);
            assert_eq!(command.get_program(), "docker");
            let source = command
                .get_current_dir()
                .ok_or(ProcessError::InvalidOutput)?;
            assert_ne!(source, root);
            assert_eq!(
                command
                    .get_envs()
                    .find_map(|(key, value)| (key == "MISE_TRUSTED_CONFIG_PATHS").then_some(value)),
                Some(Some(source.as_os_str()))
            );
            assert_eq!(
                std::fs::read(source.join("crates/core/src/lib.rs"))?,
                b"fixture"
            );
            executed = Some(source.to_owned());
            failed_status()
        });
        assert!(result.is_err());
        assert_eq!(calls, 1);
        let source =
            executed.ok_or_else(|| std::io::Error::other("native source was not executed"))?;
        assert!(!source.exists());
        assert!(destination.join("SOURCE.sha256").is_file());
        assert!(destination.join("clang-version.txt").is_file());
        assert_eq!(
            std::fs::read(destination.join("source/crates/core/src/lib.rs"))?,
            b"fixture"
        );
        assert!(capture
            .run_with(&destination, IMAGE, |_, _, _| Ok(()))
            .is_err());
        Ok(())
    })
}

fn failed_status() -> Result<(), ProcessError> {
    xtask::process::run(
        std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "native_status_fixture"])
            .env("RUSTERISK_NATIVE_STATUS", "fail")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
        std::time::Duration::from_secs(2),
    )
}

#[test]
fn native_status_fixture() -> Result<(), String> {
    if std::env::var("RUSTERISK_NATIVE_STATUS").as_deref() == Ok("fail") {
        Err("requested native command failure".to_owned())
    } else {
        Ok(())
    }
}

#[test]
fn native_execution_checks_captured_input_even_after_a_command_failure(
) -> Result<(), xtask::distribution::SdkCandidateBuildError> {
    fixture::using_project("native-corruption", |root, parent| {
        let capture = CapturedNative::read(root).map_err(std::io::Error::other)?;
        let result = capture.run_with(&parent.join("native-result"), IMAGE, |command, _, _| {
            let source = command
                .get_current_dir()
                .ok_or(ProcessError::InvalidOutput)?;
            std::fs::write(
                source.join("crates/core/src/lib.rs"),
                b"corrupted execution input",
            )?;
            failed_status()
        });
        let error = result
            .err()
            .ok_or_else(|| std::io::Error::other("changed native source was accepted"))?
            .to_string();
        assert!(error.contains("source changed"), "{error}");
        assert!(error.contains("process failed"), "{error}");
        Ok(())
    })
}

#[test]
fn interrupted_native_work_cleans_the_named_container_and_preserves_cleanup_failure(
) -> Result<(), xtask::distribution::SdkCandidateBuildError> {
    fixture::using_project("native-interruption", |root, parent| {
        let capture = CapturedNative::read(root).map_err(std::io::Error::other)?;
        let mut calls = 0;
        let mut name = None;
        let mut source = None;
        let result = capture.run_with(
            &parent.join("native-result"),
            IMAGE,
            |command, budget, mode| {
                calls += 1;
                let arguments: Vec<_> = command.get_args().collect();
                match mode {
                    CommandMode::Work => {
                        assert_eq!(budget, std::time::Duration::from_secs(50));
                        assert_eq!(
                            arguments.first().copied(),
                            Some(std::ffi::OsStr::new("run"))
                        );
                        name = arguments
                            .windows(2)
                            .find(|pair| pair[0] == "--name")
                            .map(|pair| pair[1].to_owned());
                        source = command.get_current_dir().map(std::path::Path::to_owned);
                        Err(ProcessError::Interrupted)
                    }
                    CommandMode::Cleanup => {
                        assert_eq!(budget, std::time::Duration::from_secs(30));
                        assert_eq!(
                            arguments,
                            [
                                std::ffi::OsStr::new("rm"),
                                std::ffi::OsStr::new("--force"),
                                name.as_deref().ok_or(ProcessError::InvalidOutput)?
                            ]
                        );
                        Err(std::io::Error::other("fixture container removal failed").into())
                    }
                }
            },
        );
        assert_eq!(calls, 2);
        assert!(name.is_some());
        assert!(source.is_some_and(|source| !source.exists()));
        let error = result
            .err()
            .ok_or_else(|| std::io::Error::other("interruption was accepted"))?
            .to_string();
        assert!(error.contains("process interrupted"), "{error}");
        assert!(
            error.contains("fixture container removal failed"),
            "{error}"
        );
        Ok(())
    })
}

#[test]
fn successful_process_status_cannot_replace_the_compiler_identity_check(
) -> Result<(), xtask::distribution::SdkCandidateBuildError> {
    fixture::using_project("native-compiler", |root, parent| {
        let capture = CapturedNative::read(root).map_err(std::io::Error::other)?;
        let mut calls = 0;
        let result = capture.run_with(&parent.join("native-result"), IMAGE, |_, _, _| {
            calls += 1;
            Ok(())
        });
        assert_eq!(calls, 1);
        let error = result
            .err()
            .ok_or_else(|| std::io::Error::other("missing compiler identity was accepted"))?
            .to_string();
        assert!(error.contains("compiler identity"), "{error}");
        Ok(())
    })
}
