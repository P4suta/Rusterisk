use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
mod interop;
mod security;

#[derive(Debug)]
enum TaskError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Contract(String),
    Process(xtask::process::ProcessError),
    ReferenceCleanup {
        operation: xtask::process::ProcessError,
        cleanup: xtask::process::ProcessError,
    },
}

impl std::fmt::Display for TaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Parse(error) => write!(f, "{error}"),
            Self::Contract(message) => f.write_str(message),
            Self::Process(error) => write!(f, "{error}"),
            Self::ReferenceCleanup { operation, cleanup } => {
                write!(
                    f,
                    "image build failed: {operation}, builder cleanup failed: {cleanup}"
                )
            }
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
    let arguments: Vec<String> = std::env::args().skip(1).take(6).collect();
    match arguments.as_slice() {
        [task, flag, archive] if task == "vendor-patch" && flag == "--archive" => {
            let count = xtask::vendor::patch(&std::env::current_dir()?, Path::new(archive))
                .map_err(|error| TaskError::Contract(error.to_string()))?;
            println!("vendor: recorded changes to {count} published source files");
            Ok(())
        }
        [task, flag, archive] if task == "vendor-import" && flag == "--archive" => {
            let count = xtask::vendor::import(&std::env::current_dir()?, Path::new(archive))
                .map_err(|error| TaskError::Contract(error.to_string()))?;
            println!("vendor: imported {count} published source files");
            Ok(())
        }
        [task] if task == "vendor-check" => {
            let count = xtask::vendor::check(&std::env::current_dir()?)
                .map_err(|error| TaskError::Contract(error.to_string()))?;
            println!("vendor: {count} published source files verified");
            Ok(())
        }
        [task] if task == "browser-types" => xtask::browser::prepare_types(&std::env::current_dir()?).map_err(|error| TaskError::Contract(error.to_string())),
        [task, mode, flag, image] if task == "browser" && flag == "--image" => {
            let execution = match mode.as_str() {
                "--dry-run" => xtask::browser::Execution::DryRun,
                "--run" => xtask::browser::Execution::Run,
                _ => return Err(TaskError::Contract("browser requires --dry-run or --run".into())),
            };
            xtask::browser::execute(execution, image).map_err(|error| TaskError::Contract(error.to_string()))
        }
        [task, scope, mode, flag, image] if task == "input-fuzz" && flag == "--image" => {
            let scope = xtask::input_fuzz::Scope::parse(scope).map_err(|error| TaskError::Contract(error.to_string()))?;
            let execution = match mode.as_str() {
                "--dry-run" => xtask::input_fuzz::Execution::DryRun,
                "--run" => xtask::input_fuzz::Execution::Run,
                _ => return Err(TaskError::Contract("input-fuzz requires --dry-run or --run".into())),
            };
            xtask::input_fuzz::execute(scope, execution, image).map_err(|error| TaskError::Contract(error.to_string()))
        }
        [task, mode, flag, image] if (task == "core-miri" || task == "core-mutation") && flag == "--image" => {
            let kind = if task == "core-miri" { xtask::core_assurance::Task::Miri } else { xtask::core_assurance::Task::Mutation };
            let execution = match mode.as_str() {
                "--dry-run" => xtask::core_assurance::Execution::DryRun,
                "--run" => xtask::core_assurance::Execution::Run,
                _ => return Err(TaskError::Contract(format!("{task} requires --dry-run or --run"))),
            };
            xtask::core_assurance::execute(kind, execution, image).map_err(|error| TaskError::Contract(error.to_string()))
        }
        [task, mode, flag, image] if task == "core-proofs" && flag == "--image" => {
            let execution = match mode.as_str() {
                "--dry-run" => xtask::core_proofs::Execution::DryRun,
                "--run" => xtask::core_proofs::Execution::Run,
                _ => return Err(TaskError::Contract("core-proofs requires --dry-run or --run".into())),
            };
            xtask::core_proofs::execute(execution, image).map_err(|error| TaskError::Contract(error.to_string()))
        }
        [task] if task == "interop" => interop::execute(true),
        [task, mode] if task == "interop" && mode == "--dry-run" => interop::execute(false),
        [task, mode] if task == "reference-build" && mode == "--dry-run" => reference_build(false),
        [task, mode] if task == "reference-build" && mode == "--run" => reference_build(true),
        [task, mode] if task == "verification-build" && mode == "--dry-run" => verification_build(false),
        [task, mode] if task == "verification-build" && mode == "--run" => verification_build(true),
        [task, mode] if task == "core-proofs-build" && mode == "--dry-run" => core_proof_build(false),
        [task, mode] if task == "core-proofs-build" && mode == "--run" => core_proof_build(true),
        [task, mode] if task == "browser-build" && mode == "--dry-run" => browser_build(false),
        [task, mode] if task == "browser-build" && mode == "--run" => browser_build(true),
        [task] if task == "policy" => policy(),
        [task] if task == "core-msrv" => core_msrv(),
        [task] if task == "node-check" => node_check(),
        [task] if task == "browser-check" => xtask::browser::check(&std::env::current_dir()?).map_err(|error| TaskError::Contract(error.to_string())),
        [task] if task == "distribution-check" => distribution_check(),
        [task, scope, operation] if task == "distribution" && scope == "sdk" && operation == "plan" => xtask::distribution::plan_sdk(&std::env::current_dir()?).map_err(|error| TaskError::Contract(error.to_string())),
        [task, scope, operation] if task == "distribution" && scope == "sdk" && operation == "build" => xtask::distribution::build_sdk(&std::env::current_dir()?).map(|_| ()).map_err(|error| TaskError::Contract(error.to_string())),
        [task] if task == "check" => {
            distribution_check()?;
            xtask::vendor::check(&std::env::current_dir()?).map_err(|error| TaskError::Contract(error.to_string()))?;
            run("cargo", xtask::WORKSPACE_FORMAT_CHECK)?;
            run("cargo", &["fmt", "--manifest-path", "xtask/fuzz/Cargo.toml", "--", "--check"])?;
            policy()?;
            core_msrv()?;
            node_check()?;
            xtask::browser::check(&std::env::current_dir()?).map_err(|error| TaskError::Contract(error.to_string()))?;
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
            run("cargo", &["test", "--workspace", "--locked"])?;
            run("cargo", &["clippy", "--manifest-path", "xtask/fuzz/Cargo.toml", "--target-dir", "target", "--locked", "--all-targets", "--all-features", "--", "-D", "warnings"])?;
            run("cargo", &["test", "--manifest-path", "xtask/fuzz/Cargo.toml", "--target-dir", "target", "--locked", "--features", "signaling,media", "--tests"])
        }
        [task] if task == "security" => {
            xtask::vendor::check(&std::env::current_dir()?).map_err(|error| TaskError::Contract(error.to_string()))?;
            security::check()?;
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
            "usage: cargo xtask <check|policy|core-msrv|node-check|browser-types|browser-check|distribution-check|distribution sdk <plan|build>|security|vendor-check|(vendor-import|vendor-patch) --archive <path>|(reference-build|verification-build|core-proofs-build|browser-build) <--dry-run|--run>|interop [--dry-run]|(core-proofs|core-miri|core-mutation|browser) <--dry-run|--run> --image <name@sha256:digest|sha256:image-id>|input-fuzz <all|settings|media_controls|sip|media_packets> <--dry-run|--run> --image <name@sha256:digest|sha256:image-id>>".into(),
        )),
    }
}

fn distribution_check() -> Result<(), TaskError> {
    let root = std::env::current_dir()?;
    xtask::distribution::check_sdk_inputs(&root)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    xtask::distribution::check_sdk_archive_inputs(&root)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    xtask::distribution::check_dependency_paths(&root)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    println!("distribution: SDK package inputs verified");
    Ok(())
}

fn node_check() -> Result<(), TaskError> {
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    run(
        npm,
        &[
            "ci",
            "--prefix",
            "tests/interop/node-ari-client",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
        ],
    )?;
    run(
        npm,
        &["--prefix", "tests/interop/node-ari-client", "run", "check"],
    )?;
    run(npm, &["--prefix", "tests/interop/node-ari-client", "test"])
}

fn execution_identity(execute: bool) -> Result<String, TaskError> {
    Ok(if execute {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| TaskError::Contract(error.to_string()))?;
        format!("{}-{}", std::process::id(), timestamp.as_millis())
    } else {
        "preview".into()
    })
}

fn reference_build(execute: bool) -> Result<(), TaskError> {
    let identity = execution_identity(execute)?;
    let plan = xtask::reference::ReferenceBuild::new(&identity)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    image_build(execute, plan.steps(), plan.cleanup())
}

fn verification_build(execute: bool) -> Result<(), TaskError> {
    let identity = execution_identity(execute)?;
    let plan = xtask::reference::VerificationBuild::new(&identity)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    image_build(execute, plan.steps(), plan.cleanup())
}

fn core_proof_build(execute: bool) -> Result<(), TaskError> {
    let identity = execution_identity(execute)?;
    let plan = xtask::reference::CoreProofBuild::new(&identity)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    image_build(execute, plan.steps(), plan.cleanup())
}

fn browser_build(execute: bool) -> Result<(), TaskError> {
    let identity = execution_identity(execute)?;
    let plan = xtask::browser::Build::new(&identity)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    image_build(execute, plan.steps(), plan.cleanup())
}

fn image_build(
    execute: bool,
    steps: Vec<xtask::reference::Step>,
    cleanup: xtask::reference::Step,
) -> Result<(), TaskError> {
    for step in steps.iter().chain(std::iter::once(&cleanup)) {
        println!(
            "budget={}s docker {}",
            step.budget.as_secs(),
            step.arguments.join(" ")
        );
    }
    if !execute {
        return Ok(());
    }
    let perform = |step: &xtask::reference::Step| {
        xtask::process::run(Command::new("docker").args(&step.arguments), step.budget)
    };
    if steps.is_empty() {
        return Err(TaskError::Contract("image build has no steps".into()));
    }
    let operation = steps.iter().try_for_each(perform);
    let cleanup = perform(&cleanup);
    match (operation, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(operation), Err(cleanup)) => Err(TaskError::ReferenceCleanup { operation, cleanup }),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(TaskError::Process(error)),
    }
}

fn core_msrv() -> Result<(), TaskError> {
    run(
        "rustup",
        &[
            "run",
            "1.89.0",
            "cargo",
            "test",
            "--package",
            "rusterisk-core",
            "--locked",
            "--target-dir",
            "target/core-msrv",
        ],
    )
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
