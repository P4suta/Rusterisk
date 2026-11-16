use super::{
    native::{combine, native_target, verify_loaded_server_with},
    ServerCandidate,
};
use crate::{
    distribution::{PreparedSdkHttpConsumer, SdkContents, SdkHttpTarget},
    process::CommandSupervisor,
};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn verify_server_sdk_with(
    root: &Path,
    server_candidate: &Path,
    sdk_candidate: &Path,
    supervisor: &mut CommandSupervisor,
) -> io::Result<PathBuf> {
    let server = ServerCandidate::read(server_candidate, native_target()?)?;
    let contents = SdkContents::read(sdk_candidate).map_err(io::Error::other)?;
    if server.name().version() != contents.version() {
        return Err(io::Error::other("server and SDK candidate versions differ"));
    }
    let mut parent = std::fs::canonicalize(root)?;
    for component in [".cache", "distributions", "server-sdk"] {
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
    let consumer_execution = temporary.join(format!("rusterisk-server-sdk-consumer-{identity}"));
    let server_execution = temporary.join(format!("rusterisk-server-sdk-server-{identity}"));
    println!("server SDK artifacts: {}", artifacts.display());
    println!("server source: {}", server.source_digest());
    println!("SDK source: {}", contents.source_digest());
    io::stdout().flush()?;
    let mut prepared = Some(
        PreparedSdkHttpConsumer::prepare_with(
            &contents,
            &consumer_execution,
            &artifacts.join("consumer"),
            |command, budget| supervisor.run(command, budget),
        )
        .map_err(io::Error::other)?,
    );
    let operation = verify_loaded_server_with(
        &server,
        &server_execution,
        &artifacts.join("server"),
        supervisor,
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
                .ok_or_else(|| io::Error::other("SDK consumer ownership ended"))?
                .run_with(&target, |command, budget| service.run(command, budget))
                .map(|_| ())
                .map_err(io::Error::other)
        },
    );
    let cleanup = match prepared {
        Some(prepared) => prepared.close().map_err(io::Error::other),
        None => Ok(()),
    };
    combine(operation, cleanup, "prepared SDK consumer cleanup")?;
    println!("server and SDK candidate HTTP interoperability passed");
    Ok(artifacts)
}
