use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

#[derive(Debug)]
enum TaskError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Contract(String),
}

impl std::fmt::Display for TaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Parse(error) => write!(f, "{error}"),
            Self::Contract(message) => f.write_str(message),
        }
    }
}

impl From<std::io::Error> for TaskError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<toml::de::Error> for TaskError {
    fn from(value: toml::de::Error) -> Self {
        Self::Parse(value)
    }
}

fn sources(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), TaskError> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_symlink() {
            return Err(TaskError::Contract(format!(
                "source symlink refused: {}",
                path.display()
            )));
        }
        if kind.is_dir() {
            sources(&path, files)?;
        } else if path.extension().is_some_and(|value| value == "rs") {
            if files.len() >= 4096 || entry.metadata()?.len() > 1024 * 1024 {
                return Err(TaskError::Contract(
                    "source inspection capacity exceeded".into(),
                ));
            }
            files.push(path);
        }
    }
    Ok(())
}

fn policy() -> Result<(), TaskError> {
    let exceptions: xtask::Exceptions =
        toml::from_str(&std::fs::read_to_string("api/policy-exceptions.toml")?)?;
    if exceptions.exceptions.len() > 16 {
        return Err(TaskError::Contract("exception capacity exceeded".into()));
    }
    for exception in &exceptions.exceptions {
        if !Path::new(&exception.path).is_file()
            || !exception.adr.starts_with("docs/adr/")
            || exception.adr.contains("..")
            || !Path::new(&exception.adr).is_file()
            || exception.rationale.is_empty()
        {
            return Err(TaskError::Contract("invalid exception scope or ADR".into()));
        }
    }
    let manifest: toml::Value = toml::from_str(&std::fs::read_to_string("Cargo.toml")?)?;
    let members = manifest["workspace"]["members"]
        .as_array()
        .ok_or_else(|| TaskError::Contract("workspace members missing".into()))?;
    let mut files = Vec::new();
    for member in members {
        let member = member
            .as_str()
            .ok_or_else(|| TaskError::Contract("invalid workspace member".into()))?;
        sources(Path::new(member), &mut files)?;
    }
    if files.is_empty() {
        return Err(TaskError::Contract("no Rust source inspected".into()));
    }
    let mut count = 0;
    for path in &files {
        let source = std::fs::read_to_string(path)?;
        let scoped: Vec<xtask::Exception> = exceptions
            .exceptions
            .iter()
            .filter(|exception| Path::new(&exception.path) == path)
            .cloned()
            .collect();
        let findings = xtask::inspect_with_exceptions(&source, &scoped)
            .map_err(|error| TaskError::Contract(format!("{}: {error}", path.display())))?;
        for finding in findings {
            eprintln!("{}: {}: {}", path.display(), finding.rule, finding.detail);
            count += 1;
        }
    }
    if count > 0 {
        return Err(TaskError::Contract(format!("{count} policy violations")));
    }
    println!("policy: {} Rust files inspected", files.len());
    Ok(())
}

fn run(program: &str, arguments: &[&str]) -> Result<(), TaskError> {
    println!("check: {program} {}", arguments.join(" "));
    let status = Command::new(program).args(arguments).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(TaskError::Contract(format!("{program} failed: {status}")))
    }
}

fn execute() -> Result<(), TaskError> {
    let arguments: Vec<String> = std::env::args().skip(1).take(4).collect();
    match arguments.as_slice() {
        [task] if task == "policy" => policy(),
        [task] if task == "check" => {
            run("cargo", &["fmt", "--all", "--", "--check"])?;
            policy()?;
            run(
                "cargo",
                &[
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            run(
                "cargo",
                &["check", "--workspace", "--all-targets", "--locked"],
            )?;
            run("cargo", &["test", "--workspace", "--locked"])
        }
        [task] if task == "security" => {
            run("cargo", &["deny", "check"])?;
            run(
                "gitleaks",
                &[
                    "dir",
                    ".",
                    "--config",
                    "api/gitleaks.toml",
                    "--no-banner",
                    "--redact",
                ],
            )
        }
        _ => Err(TaskError::Contract(
            "usage: cargo xtask <check|policy|security>".into(),
        )),
    }
}

fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
