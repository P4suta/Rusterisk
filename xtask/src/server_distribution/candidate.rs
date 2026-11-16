use super::{
    check_server_tar_gzip, check_server_zip, write_server_tar_gzip, write_server_zip,
    ArchiveFormat, CapturedServer, ServerBuildPhase, ServerBuildPlan, ServerPayload,
};
use crate::process::ProcessError;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    path::{Component, Path},
    process::{Command, Stdio},
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

fn bounded(root: &Path, relative: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut path = root.to_owned();
    if !std::fs::symlink_metadata(&path)?.is_dir() {
        return Err(io::Error::other("server build state changed"));
    }
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(io::Error::other(
                "server build output path escapes its state",
            ));
        };
        path.push(name);
        if path != root.join(relative) && !std::fs::symlink_metadata(&path)?.is_dir() {
            return Err(io::Error::other("server build output parent changed"));
        }
    }
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(io::Error::other(
            "server build output requires bounded regular bytes",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other(
            "server build output exceeds its byte limit",
        ));
    }
    Ok(bytes)
}

fn combine(primary: io::Result<()>, following: io::Result<()>, context: &str) -> io::Result<()> {
    match (primary, following) {
        (result, Ok(())) => result,
        (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(following)) => Err(io::Error::other(format!(
            "{primary}; {context}: {following}"
        ))),
    }
}

fn preserve_logs(state: &Path, destination: &Path) -> io::Result<()> {
    let mut result = Ok(());
    for phase in ServerBuildPhase::ALL {
        let name = format!("{}.txt", phase.label());
        let operation = match std::fs::symlink_metadata(state.join(&name)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
            Ok(_) => bounded(state, Path::new(&name), 4 * 1024 * 1024)
                .and_then(|bytes| write_new(&destination.join(name), &bytes)),
        };
        result = combine(result, operation, "server log preservation failed");
    }
    result
}

impl CapturedServer {
    pub fn build_with(
        &self,
        destination: &Path,
        mut perform: impl FnMut(&mut Command, Duration) -> Result<(), ProcessError>,
    ) -> io::Result<()> {
        std::fs::create_dir(destination)?;
        self.write(
            &destination.join("source"),
            &destination.join("captured-browser"),
        )?;
        write_new(
            &destination.join("SOURCE.sha256"),
            format!("{}\n", self.source_digest()).as_bytes(),
        )?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let directory = std::fs::canonicalize(std::env::temp_dir())?.join(format!(
            "rusterisk-server-package-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&directory)?;
        let source = directory.join("source");
        let state = directory.join("state");
        let operation = (|| {
            self.write(&source, &state)?;
            let plan = ServerBuildPlan::new(&source, &state, self.name().target())?;
            for phase in ServerBuildPhase::ALL {
                let output = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(state.join(format!("{}.txt", phase.label())))?;
                let step = plan.step(phase);
                let mut command = Command::new("cargo");
                command
                    .args(step.arguments)
                    .current_dir(&source)
                    .env("CARGO_TARGET_DIR", state.join("target"))
                    .env("CARGO_BUILD_JOBS", "2")
                    .env("MISE_TRUSTED_CONFIG_PATHS", &source)
                    .env(
                        "CARGO_NET_OFFLINE",
                        match phase {
                            ServerBuildPhase::Fetch => "false",
                            ServerBuildPhase::Build => "true",
                        },
                    )
                    .stdin(Stdio::null())
                    .stdout(output.try_clone()?)
                    .stderr(output);
                let started = std::time::Instant::now();
                let result = perform(&mut command, step.budget).map_err(io::Error::other);
                println!(
                    "server phase={} elapsed={:.3}s",
                    phase.label(),
                    started.elapsed().as_secs_f64()
                );
                combine(result, self.check(&source), "server source check failed")?;
            }
            self.collect(&plan, destination)
        })();
        let operation = combine(
            operation,
            preserve_logs(&state, destination),
            "server log preservation failed",
        );
        combine(
            operation,
            std::fs::remove_dir_all(&directory),
            "server execution directory cleanup failed",
        )
    }

    fn collect(&self, plan: &ServerBuildPlan, destination: &Path) -> io::Result<()> {
        let executable = plan.executable();
        let relative = executable
            .strip_prefix(plan.state())
            .map_err(io::Error::other)?;
        let binary = bounded(plan.state(), relative, 256 * 1024 * 1024)?;
        let payload = ServerPayload::capture(self.name().target(), binary, |path| {
            self.files()
                .find(|(file, _)| *file == Path::new(path))
                .map(|(_, bytes)| bytes.to_vec())
                .ok_or_else(|| io::Error::other("captured server input missing"))
        })
        .map_err(io::Error::other)?;
        let mut archive = Vec::new();
        match self.name().target().format() {
            ArchiveFormat::TarGzip => {
                write_server_tar_gzip(&payload, &mut archive)?;
                check_server_tar_gzip(&payload, &archive)?;
            }
            ArchiveFormat::Zip => {
                write_server_zip(&payload, &mut archive)?;
                check_server_zip(&payload, &archive)?;
            }
        }
        let digest: String = Sha256::digest(&archive)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        write_new(&destination.join(self.name().file_name()), &archive)?;
        write_new(
            &destination.join("SHA256SUMS"),
            format!("{digest}  {}\n", self.name().file_name()).as_bytes(),
        )
    }
}
