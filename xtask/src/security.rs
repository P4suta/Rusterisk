use super::{run, TaskError};
use std::path::Path;
use std::process::Command;

fn snapshot(root: &Path) -> Result<(), TaskError> {
    let database = root.join("advisories");
    let path = database
        .to_str()
        .ok_or_else(|| TaskError::Contract("advisory cache path must be UTF-8".into()))?;
    let source = std::fs::read_to_string("deny.toml")?;
    let policy = xtask::advisory_snapshot_config(&source, path)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    let rendered =
        toml::to_string(&policy).map_err(|error| TaskError::Contract(error.to_string()))?;
    let config = root.join("deny.toml");
    std::fs::write(&config, rendered)?;
    let config = config
        .to_str()
        .ok_or_else(|| TaskError::Contract("security configuration path must be UTF-8".into()))?;
    run(
        "cargo",
        &[
            "deny",
            "--config",
            config,
            "fetch",
            "db",
            "index",
            "std-replacement",
        ],
    )?;
    let mut count = 0;
    for entry in std::fs::read_dir(&database)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            count += 1;
            if count > 4 {
                return Err(TaskError::Contract(
                    "advisory database capacity exceeded".into(),
                ));
            }
            let revision = Command::new("git")
                .arg("-C")
                .arg(entry.path())
                .args(["rev-parse", "HEAD"])
                .output()?;
            if !revision.status.success() {
                return Err(TaskError::Contract(
                    "advisory revision inspection failed".into(),
                ));
            }
            let revision = String::from_utf8(revision.stdout)
                .map_err(|error| TaskError::Contract(error.to_string()))?;
            let revision = revision.trim();
            if !matches!(revision.len(), 40 | 64)
                || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(TaskError::Contract(
                    "invalid advisory source revision".into(),
                ));
            }
            println!("advisory snapshot: {revision}");
        }
    }
    if count == 0 {
        return Err(TaskError::Contract("no advisory database inspected".into()));
    }
    run(
        "cargo",
        &[
            "deny",
            "--config",
            config,
            "--offline",
            "--locked",
            "check",
            "--hide-inclusion-graph",
        ],
    )
}

pub(super) fn check() -> Result<(), TaskError> {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    let parent = std::env::current_dir()?.join(".cache");
    std::fs::create_dir_all(&parent)?;
    let root = parent.join(format!(
        "security-{}-{}",
        std::process::id(),
        time.as_nanos()
    ));
    std::fs::create_dir(&root)?;
    let result = snapshot(&root);
    let cleanup = std::fs::remove_dir_all(&root);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(primary), Err(cleanup)) => Err(TaskError::Contract(format!(
            "{primary}; advisory cache cleanup failed: {cleanup}"
        ))),
    }
}
