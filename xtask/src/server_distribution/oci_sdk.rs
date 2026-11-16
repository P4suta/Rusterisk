use super::{
    native::{combine, output, write},
    ServerCandidate, ServerRuntimeConfig, ServerTarget,
};
use crate::distribution::{PreparedSdkHttpConsumer, SdkContents, SdkHttpTarget};
use std::{
    io::{self, Read, Write},
    net::{TcpListener, UdpSocket},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

mod docker;
mod runtime;
use super::{ServerOciRunPhase, ServerOciRuntimePlan};
use crate::process::CommandSupervisor;
use runtime::{runtime_using, Runtime};

fn loaded_identity(bytes: &[u8], image: &str) -> io::Result<()> {
    if bytes != format!("{image}\n").as_bytes() {
        return Err(io::Error::other("loaded OCI image identity differs"));
    }
    Ok(())
}

fn copy_archive(source: &Path, destination: &Path, maximum: u64) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum {
        return Err(io::Error::other(
            "OCI runtime requires a bounded regular archive",
        ));
    }
    let mut source = std::fs::File::open(source)?;
    if !source.metadata()?.is_file() || source.metadata()?.len() != metadata.len() {
        return Err(io::Error::other("OCI archive changed before copying"));
    }
    let mut destination = output(destination)?;
    let copied = io::copy(&mut Read::take(&mut source, maximum + 1), &mut destination)?;
    if copied != metadata.len() || copied > maximum {
        return Err(io::Error::other("OCI archive changed while copying"));
    }
    destination.flush()?;
    let mut permissions = destination.metadata()?.permissions();
    permissions.set_readonly(true);
    destination.set_permissions(permissions)
}

pub fn verify_server_oci_sdk_with(
    root: &Path,
    server_candidate: &Path,
    target: ServerTarget,
    archive: &Path,
    sdk_candidate: &Path,
    supervisor: &mut CommandSupervisor,
) -> io::Result<PathBuf> {
    let server = ServerCandidate::read(server_candidate, target)?;
    let contents = SdkContents::read(sdk_candidate).map_err(io::Error::other)?;
    super::ServerOciProfile::new(target, server.name().version(), server.source_digest())?;
    if server.name().version() != contents.version() {
        return Err(io::Error::other(
            "OCI server and SDK candidate versions differ",
        ));
    }
    let mut parent = std::fs::canonicalize(root)?;
    for component in [".cache", "distributions", "server-oci-sdk"] {
        parent.push(component);
        super::command::ensure_directory(&parent)?;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let identity = format!("{}-{stamp}", std::process::id());
    let artifacts = parent.join(&identity);
    std::fs::create_dir(&artifacts)?;
    let temporary = std::fs::canonicalize(std::env::temp_dir())?;
    let execution = temporary.join(format!("rusterisk-server-oci-runtime-{identity}"));
    let consumer_execution = temporary.join(format!("rusterisk-server-oci-consumer-{identity}"));
    std::fs::DirBuilder::new().mode(0o700).create(&execution)?;
    let mut prepared = None;
    let operation = (|| {
        println!("server OCI SDK artifacts: {}", artifacts.display());
        println!("server source: {}", server.source_digest());
        println!("SDK source: {}", contents.source_digest());
        io::stdout().flush()?;
        let copied = execution.join("image.oci.tar");
        copy_archive(archive, &copied, 512 * 1024 * 1024)?;
        let checked = server.check_oci(&copied)?;
        write(
            &artifacts.join("IMAGE.id"),
            format!("{}\n", checked.config_digest()).as_bytes(),
        )?;
        write(
            &artifacts.join("SOURCE.sha256"),
            format!("{}\n", server.source_digest()).as_bytes(),
        )?;
        let http_reservation = TcpListener::bind("127.0.0.1:0")?;
        let sip_reservation = UdpSocket::bind("127.0.0.1:0")?;
        let configuration = ServerRuntimeConfig::capture(
            http_reservation.local_addr()?,
            sip_reservation.local_addr()?,
            |path| {
                server
                    .files()
                    .find(|(name, _)| *name == path)
                    .map(|(_, bytes)| bytes.to_vec())
                    .ok_or_else(|| io::Error::other("OCI runtime sample configuration missing"))
            },
        )?;
        let config = execution.join("config");
        std::fs::DirBuilder::new().mode(0o755).create(&config)?;
        for (path, bytes) in configuration.files() {
            let path = path
                .strip_prefix("config/")
                .ok_or_else(|| io::Error::other("OCI runtime configuration path changed"))?;
            let path = config.join(path);
            write(&path, bytes)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))?;
        }
        let plan = ServerOciRuntimePlan::new(&identity, checked.config_digest(), &config)?;
        prepared = Some(
            PreparedSdkHttpConsumer::prepare_with(
                &contents,
                &consumer_execution,
                &artifacts.join("consumer"),
                |command, budget| supervisor.run(command, budget),
            )
            .map_err(io::Error::other)?,
        );
        runtime_using(
            Runtime {
                plan,
                archive: &copied,
                image: checked.config_digest(),
                version: server.name().version(),
                artifacts: &artifacts,
                http_reservation: Some(http_reservation),
                sip_reservation: Some(sip_reservation),
            },
            supervisor,
            |arguments, stdout, stderr| {
                let mut command = Command::new("docker");
                command
                    .args(arguments)
                    .stdout(output(stdout)?)
                    .stderr(output(stderr)?);
                Ok(command)
            },
            |service, address| {
                let identifier = format!("sdk-{identity}");
                let target = SdkHttpTarget::new(
                    address,
                    "api",
                    "change-at-install",
                    contents.version(),
                    &identifier,
                )
                .map_err(io::Error::other)?;
                prepared
                    .take()
                    .ok_or_else(|| io::Error::other("prepared OCI SDK consumer ownership ended"))?
                    .run_with(&target, |command, budget| service.run(command, budget))
                    .map(|_| ())
                    .map_err(io::Error::other)
            },
        )
    })();
    let consumer_cleanup = match prepared {
        Some(prepared) => prepared.close().map_err(io::Error::other),
        None => Ok(()),
    };
    let operation = combine(
        operation,
        consumer_cleanup,
        "prepared OCI SDK consumer cleanup",
    );
    combine(
        operation,
        std::fs::remove_dir_all(&execution),
        "OCI private copy cleanup",
    )?;
    println!("OCI server and SDK candidate HTTP interoperability passed");
    Ok(artifacts)
}

#[cfg(test)]
mod tests;
