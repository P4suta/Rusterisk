use std::{
    io,
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

fn using_fixture(
    case: &str,
    perform: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let project = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| std::io::Error::other("missing project directory"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-vendor-secrets-{case}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let outcome = perform(project, &root);
    let cleanup = std::fs::remove_dir_all(root);
    match (outcome, cleanup) {
        (outcome, Ok(())) => outcome,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; fixture cleanup failed: {cleanup}"
        ))),
    }
}

fn require_detection(root: &Path, config: &Path) -> io::Result<()> {
    match scan(root, config) {
        Err(ProcessError::Status(status)) if status.code() == Some(1) => Ok(()),
        Ok(()) => Err(io::Error::other("gitleaks accepted a detectable fixture")),
        Err(error) => Err(io::Error::other(error)),
    }
}

#[test]
fn published_fixture_exclusions_keep_other_paths_and_metadata_secrets_detectable() -> io::Result<()>
{
    using_fixture("publisher-fixtures", |project, root| {
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
        scan(root, &config).map_err(std::io::Error::other)?;
        std::fs::copy(
            project.join("vendor/str0m/examples/key.pem"),
            root.join("unexpected.pem"),
        )?;
        require_detection(root, &config)?;
        std::fs::remove_file(root.join("unexpected.pem"))?;
        std::fs::write(
            root.join("vendor/str0m-source.toml"),
            std::fs::read(project.join("vendor/str0m/examples/key.pem"))?,
        )?;
        require_detection(root, &config)
    })
}

const SIPP_WEBSOCKET_PATHS: [&str; 4] = [
    ".cache/interop-inputs/sipp-source/sipp-3.8.0/regress/github-#0124-close/peer.py",
    ".cache/interop-inputs/sipp-source/sipp-3.8.0/regress/github-#0124-python/ws.py",
    ".cache/interop-inputs/sipp-source/sipp-3.8.0/src/websocket.cpp",
    ".cache/interop-inputs/sipp-source/sipp-3.8.0/rust/src/websocket.rs",
];
const RFC6455_NONCE: &str = "dGhlIHNhbXBsZSBub25jZQ==";

fn write_websocket_nonce(root: &Path, path: &str, nonce: &str) -> io::Result<()> {
    let target = root.join(path);
    std::fs::create_dir_all(
        target
            .parent()
            .ok_or_else(|| io::Error::other("missing WebSocket fixture directory"))?,
    )?;
    std::fs::write(target, format!("Sec-WebSocket-Key: {nonce}\n"))
}

#[test]
fn published_websocket_nonce_is_accepted_in_downloaded_sipp_sources() -> io::Result<()> {
    using_fixture("published-nonce", |project, root| {
        for path in SIPP_WEBSOCKET_PATHS {
            write_websocket_nonce(root, path, RFC6455_NONCE)?;
        }
        scan(root, &project.join("api/gitleaks.toml")).map_err(io::Error::other)
    })
}

#[test]
fn other_websocket_nonce_values_remain_detectable_in_downloaded_sipp_sources() -> io::Result<()> {
    using_fixture("unknown-nonce", |project, root| {
        let config = project.join("api/gitleaks.toml");
        let other = RFC6455_NONCE.replace("dGhl", "c29t");
        for path in SIPP_WEBSOCKET_PATHS {
            write_websocket_nonce(root, path, &other)?;
            require_detection(root, &config)?;
            std::fs::write(
                root.join(path),
                std::fs::read(project.join("vendor/str0m/examples/key.pem"))?,
            )?;
            require_detection(root, &config)?;
            std::fs::remove_file(root.join(path))?;
        }
        Ok(())
    })
}

#[test]
fn published_websocket_nonce_remains_detectable_in_other_paths() -> io::Result<()> {
    using_fixture("other-path", |project, root| {
        let config = project.join("api/gitleaks.toml");
        for path in [
            ".cache/interop-inputs/sipp-source/sipp-3.8.0/src/unexpected.cpp",
            ".cache/interop-inputs/sipp-source/sipp-3.8.0/src/websocket.cpp.backup",
            ".cache/interop-inputs/sipp-source/sipp-3.8.1/src/websocket.cpp",
            "src/websocket.cpp",
        ] {
            write_websocket_nonce(root, path, RFC6455_NONCE)?;
            require_detection(root, &config)?;
            std::fs::remove_file(root.join(path))?;
        }
        Ok(())
    })
}
