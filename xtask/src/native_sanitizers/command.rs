use super::{
    CapturedNative, CommandMode, Defect, NativeError, NativeLibrary, NativeSanitizerPlan, Phase,
    Scope,
};
use crate::process::CommandSupervisor;
use std::{
    fs,
    io::{self, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy)]
pub enum Execution {
    DryRun,
    Run,
}

fn preview(capture: &CapturedNative, image: &str) -> Result<(), NativeError> {
    let directory = std::env::temp_dir().join("rusterisk-native-sanitizers-preview");
    let plan = NativeSanitizerPlan::new(
        "preview",
        directory
            .join("source")
            .to_str()
            .ok_or_else(|| io::Error::other("native source path encoding"))?,
        directory
            .join("state")
            .to_str()
            .ok_or_else(|| io::Error::other("native state path encoding"))?,
        image,
    )?;
    println!(
        "native sanitizers: source={} scopes=media,storage,tls",
        capture.digest()
    );
    for phase in [
        Phase::ClangVersion,
        Phase::RustVersion,
        Phase::CompileProbe(Defect::Address),
        Phase::RejectProbe(Defect::Address),
        Phase::CompileProbe(Defect::Undefined),
        Phase::RejectProbe(Defect::Undefined),
        Phase::Fetch,
        Phase::Build(Scope::Media),
        Phase::Build(Scope::Storage),
        Phase::Build(Scope::Tls),
        Phase::InspectArchive(NativeLibrary::Opus),
        Phase::InspectArchive(NativeLibrary::Sqlite),
        Phase::InspectArchive(NativeLibrary::AwsLc),
        Phase::Inventory(Scope::Media),
        Phase::Run(Scope::Media),
        Phase::Inventory(Scope::Storage),
        Phase::Run(Scope::Storage),
        Phase::Inventory(Scope::Tls),
        Phase::Run(Scope::Tls),
    ] {
        let step = plan.step(phase);
        println!(
            "phase={} budget={}s docker {}",
            phase.label(),
            step.budget.as_secs(),
            step.arguments.join(" ")
        );
        let cleanup = plan.cleanup(phase);
        println!(
            "interruption cleanup: budget={}s docker {}",
            cleanup.budget.as_secs(),
            cleanup.arguments.join(" ")
        );
    }
    Ok(())
}

fn directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(io::Error::other(
            "native result parent must be a regular directory",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(path),
        Err(error) => Err(error),
    }
}

pub fn execute(root: &Path, execution: Execution, image: &str) -> Result<(), NativeError> {
    let capture = CapturedNative::read(root)?;
    if matches!(execution, Execution::DryRun) {
        return preview(&capture, image);
    }
    let mut destination = std::path::absolute(root)?;
    for component in [".cache", "native-sanitizers"] {
        destination.push(component);
        directory(&destination)?;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    destination.push(format!("{}-{stamp}", std::process::id()));
    println!(
        "native sanitizers: source={} results={}",
        capture.digest(),
        destination.display()
    );
    io::stdout().flush()?;
    let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
    let operation = capture.run_with(&destination, image, |command, budget, mode| match mode {
        CommandMode::Work => supervisor.run(command, budget),
        CommandMode::Cleanup => supervisor.run_cleanup(command, budget),
    });
    let cleanup = supervisor.finish();
    match (operation, cleanup) {
        (Ok(count), Ok(())) => {
            println!("native sanitizers: {count} boundary tests passed");
            Ok(())
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(io::Error::other(error).into()),
        (Err(operation), Err(error)) => Err(NativeError::FollowUp {
            operation: operation.to_string(),
            context: "native supervisor cleanup failed",
            error: io::Error::other(error),
        }),
    }
}
