use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use xtask::distribution::SdkConsumerPlan;
use xtask::server_distribution::NativeServerBuildLimits;
mod interop;
mod release_cli;
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

fn relative_path_matches(value: &str, expected: &Path) -> bool {
    let mut path = PathBuf::new();
    for component in Path::new(value).components() {
        match component {
            std::path::Component::Normal(part) => path.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !path.pop() {
                    return false;
                }
            }
            std::path::Component::Prefix(_) | std::path::Component::RootDir => return false,
        }
    }
    path == expected
}

fn windows_job_accounting_policy(
    workspace_source: &str,
    members: &[toml::Value],
    exceptions: &[xtask::Exception],
) -> Result<(), TaskError> {
    const MEMBER: &str = "crates/windows-job-accounting";
    const MANIFEST: &str = "crates/windows-job-accounting/Cargo.toml";
    const SOURCE: &str = "crates/windows-job-accounting/src/lib.rs";
    let artifact_exists = match std::fs::symlink_metadata(MEMBER) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let registered_member = members.iter().any(|member| {
        member
            .as_str()
            .is_some_and(|member| relative_path_matches(member, Path::new(MEMBER)))
    });
    let scoped: Vec<_> = exceptions
        .iter()
        .filter(|exception| relative_path_matches(&exception.path, Path::new(SOURCE)))
        .cloned()
        .collect();
    if !artifact_exists && !registered_member && scoped.is_empty() {
        return Ok(());
    }
    let manifest = std::fs::read_to_string(MANIFEST)?;
    let findings = xtask::inspect_windows_job_accounting_manifest(workspace_source, &manifest)?;
    if let Some(finding) = findings.first() {
        return Err(TaskError::Contract(format!(
            "{MANIFEST}: {}: {}",
            finding.rule, finding.detail
        )));
    }
    let source = std::fs::read_to_string(SOURCE)?;
    let findings = xtask::inspect_windows_job_accounting_source(&source, &scoped)
        .map_err(|error| TaskError::Contract(format!("{SOURCE}: {error}")))?;
    if let Some(finding) = findings.first() {
        return Err(TaskError::Contract(format!(
            "{SOURCE}: {}: {}",
            finding.rule, finding.detail
        )));
    }
    Ok(())
}

fn compiler_lint_policy(workspace: &toml::Value, members: &[toml::Value]) -> Result<(), TaskError> {
    let level = workspace
        .get("workspace")
        .and_then(|workspace| workspace.get("lints"))
        .and_then(|lints| lints.get("rust"))
        .and_then(|rust| rust.get("unsafe_code"))
        .and_then(toml::Value::as_str);
    if level != Some("forbid") {
        return Err(TaskError::Contract(
            "Cargo.toml: workspace unsafe_code must remain forbid".into(),
        ));
    }
    for member in members {
        let member = member
            .as_str()
            .ok_or_else(|| TaskError::Contract("invalid workspace member".into()))?;
        if member == "crates/windows-job-accounting" {
            continue;
        }
        let path = Path::new(member).join("Cargo.toml");
        let source = std::fs::read_to_string(&path)
            .map_err(|error| TaskError::Contract(format!("{}: {error}", path.display())))?;
        let manifest: toml::Value = toml::from_str(&source)
            .map_err(|error| TaskError::Contract(format!("{}: {error}", path.display())))?;
        let inherited = manifest
            .get("lints")
            .and_then(|lints| lints.get("workspace"))
            .and_then(toml::Value::as_bool);
        if inherited != Some(true) {
            return Err(TaskError::Contract(format!(
                "{}: members must inherit workspace lints",
                path.display()
            )));
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
    if exceptions.timers.len() > 16 {
        return Err(TaskError::Contract("timer capacity exceeded".into()));
    }
    for timer in &exceptions.timers {
        let path = Path::new(&timer.path);
        if !path.is_file()
            || !xtask::timer_policy::production(path)
            || !timer.adr.starts_with("docs/adr/")
            || timer.adr.contains("..")
            || !Path::new(&timer.adr).is_file()
            || timer.rationale.is_empty()
        {
            return Err(TaskError::Contract("invalid timer scope or ADR".into()));
        }
    }
    let workspace_source = std::fs::read_to_string("Cargo.toml")?;
    let manifest: toml::Value = toml::from_str(&workspace_source)?;
    let members = manifest["workspace"]["members"]
        .as_array()
        .ok_or_else(|| TaskError::Contract("workspace members missing".into()))?;
    compiler_lint_policy(&manifest, members)?;
    windows_job_accounting_policy(&workspace_source, members, &exceptions.exceptions)?;
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
        let mut findings = xtask::inspect_with_exceptions(&source, &scoped)
            .map_err(|error| TaskError::Contract(format!("{}: {error}", path.display())))?;
        findings.extend(
            xtask::inspect_call_turn(path, &source)
                .map_err(|error| TaskError::Contract(format!("{}: {error}", path.display())))?,
        );
        if xtask::timer_policy::production(path) {
            let timers: Vec<xtask::timer_policy::Timer> = exceptions
                .timers
                .iter()
                .filter(|timer| Path::new(&timer.path) == path)
                .cloned()
                .collect();
            findings.extend(
                xtask::timer_policy::inspect(&source, &timers)
                    .map_err(|error| TaskError::Contract(format!("{}: {error}", path.display())))?,
            );
        }
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
    let arguments: Vec<String> = std::env::args_os()
        .skip(1)
        .take(12)
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| TaskError::Contract("task arguments must be valid UTF-8".into()))
        })
        .collect::<Result<_, _>>()?;
    match arguments.as_slice() {
        #[cfg(windows)]
        [task, process_id] if task == xtask::process::WINDOWS_CONSOLE_SIGNAL_COMMAND => {
            let process_id = process_id.parse()
                .map_err(|_| TaskError::Contract("invalid owned console process".into()))?;
            xtask::process::signal_owned_console(process_id).map_err(TaskError::Io)
        }
        [task, arguments @ ..] if task == "release-check" => release_cli::execute(arguments),
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
        [task, mode, flag, image] if task == "native-sanitizers" && flag == "--image" => {
            let execution = match mode.as_str() {
                "--dry-run" => xtask::native_sanitizers::Execution::DryRun,
                "--run" => xtask::native_sanitizers::Execution::Run,
                _ => return Err(TaskError::Contract("invalid native sanitizer execution mode".into())),
            };
            xtask::native_sanitizers::execute(&std::env::current_dir()?, execution, image).map_err(|error| TaskError::Contract(error.to_string()))
        }
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
        #[cfg(unix)]
        [task, fixture, mode, image_flag, image, reference_flag, reference, extra @ ..] if task == "interop" && fixture == "browser" && image_flag == "--image" && reference_flag == "--reference-image" => {
            let execution = match mode.as_str() {
                "--dry-run" => xtask::browser::Execution::DryRun,
                "--run" => xtask::browser::Execution::Run,
                _ => return Err(TaskError::Contract("interop browser requires --dry-run or --run".into())),
            };
            let operation = match extra {
                [] => xtask::browser::execute_comparison(execution, image, reference),
                [target_flag, target, candidate_flag, candidate, order @ ..] if target_flag == "--server-target" && candidate_flag == "--server-candidate" => {
                    let order = match order {
                        [] => xtask::browser::PackagedOrder::RusteriskFirst,
                        [flag] if flag == "--reference-first" => xtask::browser::PackagedOrder::AsteriskFirst,
                        _ => return Err(TaskError::Contract("interop browser accepts only --reference-first after the server candidate".into())),
                    };
                    let target = xtask::server_distribution::ServerTarget::parse(target).map_err(|error| TaskError::Contract(error.to_string()))?;
                    xtask::browser::execute_packaged_comparison(execution, image, reference, target, Path::new(candidate), order)
                }
                _ => return Err(TaskError::Contract("interop browser requires --server-target and --server-candidate together".into())),
            };
            operation.map_err(|error| TaskError::Contract(error.to_string()))
        }
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
        [task, scope, operation] if task == "distribution" && scope == "cli" && operation == "verify" => xtask::distribution::verify_cli(&std::env::current_dir()?).map_err(TaskError::Io),
        [task, scope, operation] if task == "distribution" && scope == "sdk" && operation == "plan" => xtask::distribution::plan_sdk(&std::env::current_dir()?).map_err(|error| TaskError::Contract(error.to_string())),
        [task, scope, operation] if task == "distribution" && scope == "sdk" && operation == "build" => distribution_sdk_build(),
        [task, scope, operation, flag, candidate] if task == "distribution" && scope == "sdk" && operation == "check-content" && flag == "--candidate" => distribution_sdk_content(Path::new(candidate)),
        [task, scope, operation, flag, candidate] if task == "distribution" && scope == "sdk" && operation == "verify" && flag == "--candidate" => distribution_sdk_verify(Path::new(candidate), SdkConsumerPlan::DEFAULT_JOBS),
        [task, scope, operation, flag, candidate, jobs_flag, jobs] if task == "distribution" && scope == "sdk" && operation == "verify" && flag == "--candidate" && jobs_flag == "--consumer-jobs" => distribution_sdk_verify(Path::new(candidate), consumer_jobs(jobs)?),
        [task, scope, operation, flag, target] if task == "distribution" && scope == "server" && operation == "plan" && flag == "--target" => distribution_server_plan(target, NativeServerBuildLimits::default()),
        [task, scope, operation, flag, target] if task == "distribution" && scope == "server" && operation == "build" && flag == "--target" => distribution_server_build(target, NativeServerBuildLimits::default()),
        [task, scope, operation, target_flag, target, jobs_flag, jobs, timeout_flag, seconds] if task == "distribution" && scope == "server" && operation == "plan" && target_flag == "--target" && jobs_flag == "--jobs" && timeout_flag == "--build-timeout-seconds" => distribution_server_plan(target, native_server_build_limits(jobs, seconds)?),
        [task, scope, operation, target_flag, target, jobs_flag, jobs, timeout_flag, seconds] if task == "distribution" && scope == "server" && operation == "build" && target_flag == "--target" && jobs_flag == "--jobs" && timeout_flag == "--build-timeout-seconds" => distribution_server_build(target, native_server_build_limits(jobs, seconds)?),
        [task, scope, operation, target_flag, target, image_flag, image] if task == "distribution" && scope == "server" && operation == "plan-container" && target_flag == "--target" && image_flag == "--image" => {
            let target = xtask::server_distribution::ServerTarget::parse(target).map_err(|error| TaskError::Contract(error.to_string()))?;
            xtask::server_distribution::plan_server_container(&std::env::current_dir()?, target, image).map_err(TaskError::Io)
        },
        [task, scope, operation, target_flag, target, image_flag, image] if task == "distribution" && scope == "server" && operation == "build-container" && target_flag == "--target" && image_flag == "--image" => distribution_server_container(target, image),
        [task, scope, operation, flag, candidate] if task == "distribution" && scope == "server" && operation == "check-startup" && flag == "--candidate" => distribution_server_startup(Path::new(candidate)),
        [task, scope, operation, candidate_flag, candidate, target_flag, target] if task == "distribution" && scope == "server" && operation == "check-startup" && candidate_flag == "--candidate" && target_flag == "--target" => distribution_server_startup_target(Path::new(candidate), Some(target), false),
        [task, scope, operation, candidate_flag, candidate, target_flag, target, translation] if task == "distribution" && scope == "server" && operation == "check-startup" && candidate_flag == "--candidate" && target_flag == "--target" && translation == "--rosetta" => distribution_server_startup_target(Path::new(candidate), Some(target), true),
        [task, scope, operation, candidate_flag, candidate, sdk_flag, sdk] if task == "distribution" && scope == "server" && operation == "verify" && candidate_flag == "--candidate" && sdk_flag == "--sdk-candidate" => distribution_server_sdk(Path::new(candidate), Path::new(sdk), SdkConsumerPlan::DEFAULT_JOBS),
        [task, scope, operation, candidate_flag, candidate, sdk_flag, sdk, jobs_flag, jobs] if task == "distribution" && scope == "server" && operation == "verify" && candidate_flag == "--candidate" && sdk_flag == "--sdk-candidate" && jobs_flag == "--consumer-jobs" => distribution_server_sdk(Path::new(candidate), Path::new(sdk), consumer_jobs(jobs)?),
        [task, scope, operation, candidate_flag, candidate, target_flag, target, archive_flag, archive, sdk_flag, sdk] if task == "distribution" && scope == "server" && operation == "verify-oci" && candidate_flag == "--candidate" && target_flag == "--target" && archive_flag == "--archive" && sdk_flag == "--sdk-candidate" => distribution_server_oci_sdk(Path::new(candidate), target, Path::new(archive), Path::new(sdk)),
        [task, scope, operation, candidate_flag, candidate, target_flag, target] if task == "distribution" && scope == "server" && operation == "check-content" && candidate_flag == "--candidate" && target_flag == "--target" => {
            let target = xtask::server_distribution::ServerTarget::parse(target).map_err(|error| TaskError::Contract(error.to_string()))?;
            let candidate = xtask::server_distribution::ServerCandidate::read(Path::new(candidate), target)?;
            println!("server candidate contents passed source={} target={}", candidate.source_digest(), candidate.name().target().triple());
            Ok(())
        },
        [task, scope, operation, candidate_flag, candidate, target_flag, target] if task == "distribution" && scope == "server" && operation == "export-oci" && candidate_flag == "--candidate" && target_flag == "--target" => distribution_server_oci_export(Path::new(candidate), target),
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
            "usage: cargo xtask <check|policy|core-msrv|node-check|browser-types|browser-check|distribution-check|distribution cli verify|distribution sdk <plan|build|check-content --candidate <path>|verify --candidate <path> [--consumer-jobs <jobs>]>|distribution server <<plan|build> --target <triple> [--jobs <jobs> --build-timeout-seconds <seconds>]|<plan-container|build-container> --target <triple> --image <digest>|<check-content|export-oci> --candidate <path> --target <triple>|check-startup --candidate <path> [--target <triple> [--rosetta]]|verify --candidate <path> --sdk-candidate <path> [--consumer-jobs <jobs>]|verify-oci --candidate <path> --target <triple> --archive <path> --sdk-candidate <path>>|security|vendor-check|(vendor-import|vendor-patch) --archive <path>|(reference-build|verification-build|core-proofs-build|browser-build) <--dry-run|--run>|interop [--dry-run]|interop browser <--dry-run|--run> --image <digest> --reference-image <sha256:image-id> [--server-target <triple> --server-candidate <path> [--reference-first]]|(core-proofs|core-miri|core-mutation|browser|native-sanitizers) <--dry-run|--run> --image <name@sha256:digest|sha256:image-id>|input-fuzz <all|settings|media_controls|sip|media_packets> <--dry-run|--run> --image <name@sha256:digest|sha256:image-id>>".into(),
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
fn distribution_sdk_build() -> Result<(), TaskError> {
    let root = std::env::current_dir()?;
    let mut supervisor = xtask::process::CommandSupervisor::new().map_err(TaskError::Process)?;
    let operation = xtask::distribution::build_sdk_with(&root, |command, budget| {
        supervisor.run(command, budget)
    })
    .map(|_| ())
    .map_err(|error| TaskError::Contract(error.to_string()));
    let cleanup = supervisor.finish();
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(TaskError::Process(error)),
        (Err(operation), Err(cleanup)) => Err(TaskError::Contract(format!(
            "{operation}; SDK supervisor cleanup failed: {cleanup}"
        ))),
    }
}

fn distribution_sdk_content(candidate: &Path) -> Result<(), TaskError> {
    let contents = xtask::distribution::SdkContents::read(candidate)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    println!(
        "distribution: SDK candidate contents verified source={} version={}",
        contents.source_digest(),
        contents.version(),
    );
    Ok(())
}

fn consumer_jobs(value: &str) -> Result<NonZeroUsize, TaskError> {
    value
        .parse()
        .map_err(|_| TaskError::Contract("SDK consumer jobs must be a positive integer".into()))
}

fn distribution_sdk_verify(candidate: &Path, jobs: NonZeroUsize) -> Result<(), TaskError> {
    let root = std::env::current_dir()?;
    let mut supervisor = xtask::process::CommandSupervisor::new().map_err(TaskError::Process)?;
    let operation =
        xtask::distribution::verify_sdk_with_jobs(&root, candidate, jobs, |command, budget| {
            supervisor.run_owned(command, budget)
        })
        .map(|_| ())
        .map_err(|error| TaskError::Contract(error.to_string()));
    let cleanup = supervisor.finish();
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(TaskError::Process(error)),
        (Err(operation), Err(cleanup)) => Err(TaskError::Contract(format!(
            "{operation}; SDK consumer supervisor cleanup failed: {cleanup}"
        ))),
    }
}

fn distribution_server_sdk(
    candidate: &Path,
    sdk: &Path,
    jobs: NonZeroUsize,
) -> Result<(), TaskError> {
    #[cfg(any(unix, windows))]
    {
        let mut supervisor =
            xtask::process::CommandSupervisor::new().map_err(TaskError::Process)?;
        let operation = xtask::server_distribution::verify_server_sdk_with_jobs(
            &std::env::current_dir()?,
            candidate,
            sdk,
            jobs,
            &mut supervisor,
        )
        .map(|_| ())
        .map_err(TaskError::Io);
        match (operation, supervisor.finish()) {
            (operation, Ok(())) => operation,
            (Ok(()), Err(error)) => Err(TaskError::Process(error)),
            (Err(operation), Err(cleanup)) => Err(TaskError::Contract(format!(
                "{operation}; server and SDK supervisor cleanup failed: {cleanup}"
            ))),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (candidate, sdk, jobs);
        Err(TaskError::Process(
            xtask::process::ProcessError::UnsupportedSupervision,
        ))
    }
}

fn distribution_server_startup(candidate: &Path) -> Result<(), TaskError> {
    distribution_server_startup_target(candidate, None, false)
}

fn distribution_server_oci_sdk(
    candidate: &Path,
    target: &str,
    archive: &Path,
    sdk: &Path,
) -> Result<(), TaskError> {
    let target = xtask::server_distribution::ServerTarget::parse(target)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    #[cfg(unix)]
    {
        let mut supervisor =
            xtask::process::CommandSupervisor::new().map_err(TaskError::Process)?;
        let operation = xtask::server_distribution::verify_server_oci_sdk_with(
            &std::env::current_dir()?,
            candidate,
            target,
            archive,
            sdk,
            &mut supervisor,
        )
        .map(|_| ())
        .map_err(TaskError::Io);
        match (operation, supervisor.finish()) {
            (operation, Ok(())) => operation,
            (Ok(()), Err(error)) => Err(TaskError::Process(error)),
            (Err(operation), Err(cleanup)) => Err(TaskError::Contract(format!(
                "{operation}; OCI server and SDK supervisor cleanup failed: {cleanup}"
            ))),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (candidate, target, archive, sdk);
        Err(TaskError::Process(
            xtask::process::ProcessError::UnsupportedSupervision,
        ))
    }
}

fn distribution_server_startup_target(
    candidate: &Path,
    target: Option<&str>,
    translation: bool,
) -> Result<(), TaskError> {
    #[cfg(any(unix, windows))]
    {
        let mut supervisor =
            xtask::process::CommandSupervisor::new().map_err(TaskError::Process)?;
        let root = std::env::current_dir()?;
        let operation = match target {
            Some(target) => {
                let target = xtask::server_distribution::ServerTarget::parse(target)
                    .map_err(|error| TaskError::Contract(error.to_string()))?;
                xtask::server_distribution::check_server_startup_for_target_with(
                    &root,
                    candidate,
                    target,
                    translation,
                    &mut supervisor,
                )
            }
            None => xtask::server_distribution::check_server_startup_with(
                &root,
                candidate,
                &mut supervisor,
            ),
        }
        .map(|_| ())
        .map_err(TaskError::Io);
        match (operation, supervisor.finish()) {
            (operation, Ok(())) => operation,
            (Ok(()), Err(error)) => Err(TaskError::Process(error)),
            (Err(operation), Err(cleanup)) => Err(TaskError::Contract(format!(
                "{operation}; server startup supervisor cleanup failed: {cleanup}"
            ))),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (candidate, target, translation);
        Err(TaskError::Process(
            xtask::process::ProcessError::UnsupportedSupervision,
        ))
    }
}

fn distribution_server_oci_export(candidate: &Path, target: &str) -> Result<(), TaskError> {
    let target = xtask::server_distribution::ServerTarget::parse(target)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    let mut supervisor = xtask::process::CommandSupervisor::new().map_err(TaskError::Process)?;
    let operation = xtask::server_distribution::export_server_candidate_oci_with(
        &std::env::current_dir()?,
        candidate,
        target,
        |command, budget, mode| match mode {
            xtask::server_distribution::ServerBuildCommandMode::Work => {
                supervisor.run(command, budget)
            }
            xtask::server_distribution::ServerBuildCommandMode::Cleanup => {
                supervisor.run_cleanup(command, budget)
            }
        },
    )
    .map(|_| ())
    .map_err(TaskError::Io);
    let cleanup = supervisor.finish().map_err(TaskError::Process);
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(TaskError::Contract(format!(
            "{operation}; server OCI export supervisor cleanup failed: {cleanup}"
        ))),
    }
}

fn distribution_server_container(target: &str, image: &str) -> Result<(), TaskError> {
    let target = xtask::server_distribution::ServerTarget::parse(target)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    let mut supervisor = xtask::process::CommandSupervisor::new().map_err(TaskError::Process)?;
    let operation = xtask::server_distribution::build_server_in_container_with(
        &std::env::current_dir()?,
        target,
        image,
        |command, budget, mode| match mode {
            xtask::server_distribution::ServerBuildCommandMode::Work => {
                supervisor.run_owned(command, budget)
            }
            xtask::server_distribution::ServerBuildCommandMode::Cleanup => {
                supervisor.run_cleanup_owned(command, budget)
            }
        },
    )
    .map(|_| ())
    .map_err(TaskError::Io);
    match (operation, supervisor.finish()) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(TaskError::Process(error)),
        (Err(operation), Err(cleanup)) => Err(TaskError::Contract(format!(
            "{operation}; server container supervisor cleanup failed: {cleanup}"
        ))),
    }
}

fn native_server_build_limits(
    jobs: &str,
    seconds: &str,
) -> Result<NativeServerBuildLimits, TaskError> {
    let jobs = jobs
        .parse()
        .map_err(|_| TaskError::Contract("invalid native server build jobs".into()))?;
    let seconds = seconds
        .parse()
        .map_err(|_| TaskError::Contract("invalid native server build timeout".into()))?;
    NativeServerBuildLimits::new(jobs, seconds).map_err(TaskError::Io)
}

fn distribution_server_plan(
    target: &str,
    limits: NativeServerBuildLimits,
) -> Result<(), TaskError> {
    let target = xtask::server_distribution::ServerTarget::parse(target)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    xtask::server_distribution::plan_server_with_limits(&std::env::current_dir()?, target, limits)
        .map_err(TaskError::Io)
}

fn distribution_server_build(
    target: &str,
    limits: NativeServerBuildLimits,
) -> Result<(), TaskError> {
    let target = xtask::server_distribution::ServerTarget::parse(target)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    let mut supervisor = xtask::process::CommandSupervisor::new().map_err(TaskError::Process)?;
    let operation = xtask::server_distribution::build_server_with_limits(
        &std::env::current_dir()?,
        target,
        limits,
        |command, budget| supervisor.run_owned(command, budget),
    )
    .map(|_| ())
    .map_err(TaskError::Io);
    match (operation, supervisor.finish()) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(TaskError::Process(error)),
        (Err(operation), Err(cleanup)) => Err(TaskError::Contract(format!(
            "{operation}; server build supervisor cleanup failed: {cleanup}"
        ))),
    }
}

fn node_check() -> Result<(), TaskError> {
    node_check_with(run)
}

fn node_check_with(
    mut command: impl FnMut(&str, &[&str]) -> Result<(), TaskError>,
) -> Result<(), TaskError> {
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    command(
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
    command(
        npm,
        &["--prefix", "tests/interop/node-ari-client", "run", "check"],
    )?;
    command(npm, &["--prefix", "tests/interop/node-ari-client", "test"])
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
