use super::TaskError;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

struct DenyTemporary {
    directory: PathBuf,
}

impl DenyTemporary {
    fn create(root: &Path) -> std::io::Result<Self> {
        let directory = root.join("temporary");
        std::fs::create_dir(&directory)?;
        Ok(Self {
            directory: directory.canonicalize()?,
        })
    }

    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new("cargo");
        command
            .args(arguments)
            .env("TMPDIR", &self.directory)
            .env("TMP", &self.directory)
            .env("TEMP", &self.directory);
        command
    }

    fn run(&self, arguments: &[&str]) -> Result<(), TaskError> {
        println!("check: cargo {}", arguments.join(" "));
        let status = self.command(arguments).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(TaskError::Contract(format!("cargo failed: {status}")))
        }
    }
}

fn bounded_text(path: &Path) -> Result<String, TaskError> {
    let mut text = String::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_string(&mut text)?;
    if text.len() > 1024 * 1024 {
        return Err(TaskError::Contract(
            "dependency audit input exceeds 1 MiB".into(),
        ));
    }
    Ok(text)
}

fn legacy(root: &Path) -> Result<(), TaskError> {
    let fixture = Path::new("tests/interop/node-ari-client");
    let path = root.join("node-audit.json");
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    println!("check: npm --prefix tests/interop/node-ari-client audit --json");
    let findings = match xtask::process::run(
        Command::new(npm)
            .args([
                "--prefix",
                "tests/interop/node-ari-client",
                "audit",
                "--json",
            ])
            .stdout(output),
        Duration::from_secs(120),
    ) {
        Ok(()) => false,
        Err(xtask::process::ProcessError::Status(status)) if status.code() == Some(1) => true,
        Err(error) => return Err(TaskError::Process(error)),
    };
    let assessment = xtask::legacy_audit::assess_fixture(fixture, &bounded_text(&path)?)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    if findings != (assessment.packages > 0) {
        return Err(TaskError::Contract(
            "dependency audit status doesn't match its findings".into(),
        ));
    }
    println!(
        "legacy audit: {} scoped advisories in {} private fixture dependencies",
        assessment.advisories, assessment.packages
    );
    Ok(())
}

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
    let temporary = DenyTemporary::create(root)?;
    temporary.run(&[
        "deny",
        "--config",
        config,
        "fetch",
        "db",
        "index",
        "std-replacement",
    ])?;
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
    for manifest in ["Cargo.toml", "xtask/fuzz/Cargo.toml"] {
        temporary.run(&[
            "deny",
            "--manifest-path",
            manifest,
            "--all-features",
            "--config",
            config,
            "--offline",
            "--locked",
            "check",
            "--hide-inclusion-graph",
        ])?;
    }
    legacy(root)?;
    xtask::legacy_audit::check_browser_fixture(Path::new("examples/browser"))
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    println!("check: npm --prefix examples/browser audit --audit-level=low");
    xtask::process::run(
        Command::new(npm).args(["--prefix", "examples/browser", "audit", "--audit-level=low"]),
        Duration::from_secs(120),
    )
    .map_err(TaskError::Process)?;
    Ok(())
}

pub(super) fn check() -> Result<(), TaskError> {
    xtask::distribution::check_dependency_paths(&std::env::current_dir()?)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
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

#[cfg(test)]
mod tests;
