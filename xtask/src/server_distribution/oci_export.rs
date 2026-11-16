use super::{
    ServerBuildCommandMode, ServerCandidate, ServerOciBuildPhase, ServerOciBuildPlan,
    ServerOciContext,
};
use crate::process::ProcessError;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

fn combine(operation: io::Result<()>, following: io::Result<()>, context: &str) -> io::Result<()> {
    match (operation, following) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(following)) => Err(io::Error::other(format!(
            "{operation}; {context}: {following}"
        ))),
    }
}

fn regular(path: &Path, maximum: u64) -> io::Result<u64> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err(io::Error::other(
            "OCI output exceeds its regular-file limit",
        ));
    }
    Ok(metadata.len())
}

fn checksum(path: &Path) -> io::Result<String> {
    const MAXIMUM: u64 = 512 * 1024 * 1024;
    let expected = regular(path, MAXIMUM)?;
    if expected == 0 {
        return Err(io::Error::other("OCI export is empty"));
    }
    let mut source = std::fs::File::open(path)?.take(MAXIMUM + 1);
    let mut buffer = [0; 65_536];
    let mut digest = Sha256::new();
    let mut count = 0;
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        count += read as u64;
        digest.update(&buffer[..read]);
    }
    if count != expected || count > MAXIMUM {
        return Err(io::Error::other(
            "OCI export changed during checksum calculation",
        ));
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn perform_step(
    arguments: &[String],
    budget: Duration,
    mode: ServerBuildCommandMode,
    path: &Path,
    perform: &mut impl FnMut(&mut Command, Duration, ServerBuildCommandMode) -> Result<(), ProcessError>,
) -> io::Result<()> {
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let mut command = Command::new("docker");
    command
        .args(arguments)
        .stdout(output.try_clone()?)
        .stderr(output);
    let started = std::time::Instant::now();
    let operation = perform(&mut command, budget, mode).map_err(io::Error::other);
    println!(
        "server OCI step={} elapsed={:.3}s",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown"),
        started.elapsed().as_secs_f64()
    );
    combine(
        operation,
        regular(path, 4 * 1024 * 1024).map(|_| ()),
        "OCI log preservation failed",
    )
}

pub fn export_server_oci_with(
    candidate: &ServerCandidate,
    execution: &Path,
    artifacts: &Path,
    mut perform: impl FnMut(&mut Command, Duration, ServerBuildCommandMode) -> Result<(), ProcessError>,
) -> io::Result<PathBuf> {
    let profile = super::ServerOciProfile::new(
        candidate.name().target(),
        candidate.name().version(),
        candidate.source_digest(),
    )?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let name = format!("{}.oci.tar", candidate.name().root());
    let archive = artifacts.join(&name);
    let plan = ServerOciBuildPlan::new(
        &format!("{}-{stamp}", std::process::id()),
        execution,
        &archive,
        &profile,
    )?;
    for root in [execution, artifacts] {
        let parent = root
            .parent()
            .ok_or_else(|| io::Error::other("OCI export parent missing"))?;
        if std::fs::canonicalize(parent)? != parent {
            return Err(io::Error::other(
                "OCI exports require physical parent directories",
            ));
        }
    }
    if artifacts.starts_with(execution) || execution.starts_with(artifacts) {
        return Err(io::Error::other(
            "OCI export artifacts must remain outside their context",
        ));
    }
    std::fs::create_dir(artifacts)?;
    write_new(
        &artifacts.join("SOURCE.sha256"),
        format!("{}\n", candidate.source_digest()).as_bytes(),
    )?;
    write_new(
        &artifacts.join("Dockerfile"),
        profile.dockerfile().as_bytes(),
    )?;
    let context = ServerOciContext::create(candidate, execution)?;
    let operation = (|| {
        for phase in ServerOciBuildPhase::ALL {
            context.check()?;
            let step = plan.step(phase);
            let operation = perform_step(
                &step.arguments,
                step.budget,
                ServerBuildCommandMode::Work,
                &artifacts.join(format!("{}.txt", phase.label())),
                &mut perform,
            );
            combine(operation, context.check(), "OCI build context changed")?;
        }
        Ok(())
    })();
    let cleanup = plan.cleanup();
    let operation = combine(
        operation,
        perform_step(
            &cleanup.arguments,
            cleanup.budget,
            ServerBuildCommandMode::Cleanup,
            &artifacts.join("cleanup.txt"),
            &mut perform,
        ),
        "OCI builder cleanup failed",
    );
    let operation = combine(
        operation,
        context.check(),
        "OCI cleanup changed the context",
    );
    combine(operation, context.close(), "OCI context cleanup failed")?;
    let digest = checksum(&archive)?;
    write_new(
        &artifacts.join("SHA256SUMS"),
        format!("{digest}  {name}\n").as_bytes(),
    )?;
    Ok(archive)
}

pub fn export_server_candidate_oci_with(
    root: &Path,
    candidate: &Path,
    target: super::ServerTarget,
    perform: impl FnMut(&mut Command, Duration, ServerBuildCommandMode) -> Result<(), ProcessError>,
) -> io::Result<PathBuf> {
    let candidate = ServerCandidate::read(candidate, target)?;
    super::ServerOciProfile::new(
        target,
        candidate.name().version(),
        candidate.source_digest(),
    )?;
    let mut parent = root.to_owned();
    for component in [".cache", "distributions", "server-oci"] {
        parent.push(component);
        super::command::ensure_directory(&parent)?;
    }
    let parent = std::fs::canonicalize(parent)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let identity = format!("{}-{stamp}", std::process::id());
    let artifacts = parent.join(&identity);
    let execution = std::fs::canonicalize(std::env::temp_dir())?
        .join(format!("rusterisk-server-oci-{identity}"));
    println!(
        "server OCI export: {} source={} target={}",
        artifacts.display(),
        candidate.source_digest(),
        target.triple()
    );
    io::stdout().flush()?;
    let archive = export_server_oci_with(&candidate, &execution, &artifacts, perform)?;
    println!("server OCI archive exported: {}", archive.display());
    Ok(archive)
}
