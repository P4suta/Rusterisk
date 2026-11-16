use super::{
    CapturedServer, ServerBuildCommandMode, ServerBuildPhase, ServerBuildPlan,
    ServerContainerBuildPlan, ServerTarget,
};
use crate::process::ProcessError;
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub fn plan_server(root: &Path, target: ServerTarget) -> io::Result<()> {
    let capture = CapturedServer::read(root, target)?;
    println!(
        "server source={} target={} candidate={}",
        capture.source_digest(),
        target.triple(),
        capture.name().file_name()
    );
    let directory = std::env::temp_dir().join("rusterisk-server-preview");
    let plan = ServerBuildPlan::new(&directory.join("source"), &directory.join("state"), target)?;
    for phase in ServerBuildPhase::ALL {
        let step = plan.step(phase);
        println!(
            "phase={} budget={}s cargo {}",
            phase.label(),
            step.budget.as_secs(),
            step.arguments.join(" ")
        );
    }
    Ok(())
}

pub fn plan_server_container(root: &Path, target: ServerTarget, image: &str) -> io::Result<()> {
    let capture = CapturedServer::read(root, target)?;
    let directory = std::env::temp_dir().join("rusterisk-server-container-preview");
    let plan = ServerContainerBuildPlan::new(
        "preview",
        &directory.join("source"),
        &directory.join("state"),
        target,
        image,
    )?;
    println!(
        "server source={} target={} image={image}",
        capture.source_digest(),
        target.triple()
    );
    for phase in ServerBuildPhase::ALL {
        let step = plan.step(phase);
        println!(
            "phase={} budget={}s docker {}",
            phase.label(),
            step.budget.as_secs(),
            step.arguments.join(" ")
        );
    }
    Ok(())
}

pub(super) fn ensure_directory(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(io::Error::other(
            "server candidate parent must remain a regular directory",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => std::fs::create_dir(path),
        Err(error) => Err(error),
    }
}

pub fn build_server_with(
    root: &Path,
    target: ServerTarget,
    mut perform: impl FnMut(&mut Command, Duration) -> Result<(), ProcessError>,
) -> io::Result<PathBuf> {
    build_server_using(root, target, None, |command, budget, _| {
        perform(command, budget)
    })
}

pub fn build_server_in_container_with(
    root: &Path,
    target: ServerTarget,
    image: &str,
    perform: impl FnMut(&mut Command, Duration, ServerBuildCommandMode) -> Result<(), ProcessError>,
) -> io::Result<PathBuf> {
    let temporary = std::env::temp_dir();
    ServerContainerBuildPlan::new(
        "validation",
        &temporary.join("source"),
        &temporary.join("state"),
        target,
        image,
    )?;
    build_server_using(root, target, Some(image), perform)
}

fn build_server_using(
    root: &Path,
    target: ServerTarget,
    image: Option<&str>,
    mut perform: impl FnMut(&mut Command, Duration, ServerBuildCommandMode) -> Result<(), ProcessError>,
) -> io::Result<PathBuf> {
    let capture = CapturedServer::read(root, target)?;
    println!(
        "server source={} target={}",
        capture.source_digest(),
        target.triple()
    );
    let mut parent = root.to_owned();
    for component in [".cache", "distributions", "server"] {
        parent.push(component);
        ensure_directory(&parent)?;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let candidate = parent.join(format!("{}-{stamp}", std::process::id()));
    println!("server candidate: {}", candidate.display());
    io::stdout().flush()?;
    match image {
        Some(image) => capture.build_in_container_with(&candidate, image, perform)?,
        None => capture.build_with(&candidate, |command, budget| {
            perform(command, budget, ServerBuildCommandMode::Work)
        })?,
    }
    println!("server candidate created");
    Ok(candidate)
}
