use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use xtask::process::{run, ProcessError};

fn scan(root: &Path, config: &Path) -> Result<(), ProcessError> {
    run(
        Command::new("gitleaks")
            .args(["dir", ".", "--redact", "--no-banner", "--config"])
            .arg(config)
            .current_dir(root)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
        Duration::from_secs(20),
    )
}

#[test]
fn published_fixture_exclusions_keep_other_paths_and_metadata_secrets_detectable(
) -> Result<(), std::io::Error> {
    let project = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| std::io::Error::other("missing project directory"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-vendor-secrets-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let outcome = (|| {
        for path in [
            "vendor/str0m/cert.pem",
            "vendor/str0m/examples/key.pem",
            "vendor/str0m-source.toml",
        ] {
            let target = root.join(path);
            std::fs::create_dir_all(
                target
                    .parent()
                    .ok_or_else(|| std::io::Error::other("missing fixture directory"))?,
            )?;
            std::fs::copy(project.join(path), target)?;
        }
        let config = project.join("api/gitleaks.toml");
        scan(&root, &config).map_err(std::io::Error::other)?;
        std::fs::copy(
            project.join("vendor/str0m/examples/key.pem"),
            root.join("unexpected.pem"),
        )?;
        assert!(
            matches!(scan(&root, &config), Err(ProcessError::Status(status)) if status.code() == Some(1))
        );
        std::fs::remove_file(root.join("unexpected.pem"))?;
        std::fs::write(
            root.join("vendor/str0m-source.toml"),
            std::fs::read(project.join("vendor/str0m/examples/key.pem"))?,
        )?;
        assert!(
            matches!(scan(&root, &config), Err(ProcessError::Status(status)) if status.code() == Some(1))
        );
        Ok(())
    })();
    let cleanup = std::fs::remove_dir_all(root);
    outcome.and(cleanup)
}
