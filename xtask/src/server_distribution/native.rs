use super::{
    check_server_ready_log,
    runtime_commands::{RuntimeCommands, RuntimeService},
    ServerCandidate, ServerRuntimeConfig, ServerTarget,
};
use crate::process::{CommandStream, CommandSupervisor, RunningCommand, SupervisedCommand};
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, UdpSocket},
    path::Path,
    time::{Duration, Instant},
};

pub(super) fn native_target() -> io::Result<ServerTarget> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok(ServerTarget::MacOsArm64),
        ("macos", "x86_64") => Ok(ServerTarget::MacOsX86_64),
        ("linux", "aarch64") => Ok(ServerTarget::LinuxArm64),
        ("linux", "x86_64") => Ok(ServerTarget::LinuxX86_64),
        ("windows", "x86_64") => Ok(ServerTarget::WindowsX86_64),
        _ => Err(io::Error::other(
            "server startup requires a supported native host",
        )),
    }
}

pub(super) fn read(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(io::Error::other(
            "server runtime output exceeds its regular-file limit",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other(
            "server runtime output exceeded its byte limit",
        ));
    }
    Ok(bytes)
}

pub(super) fn output(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

pub(super) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = output(path)?;
    file.write_all(bytes)?;
    file.flush()
}

pub(super) fn combine<T>(
    operation: io::Result<T>,
    cleanup: io::Result<()>,
    context: &str,
) -> io::Result<T> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; {context}: {cleanup}"
        ))),
    }
}

pub(super) fn check_command<F>(
    supervisor: &mut RuntimeCommands<'_, F>,
    mut command: SupervisedCommand,
    artifacts: &Path,
    label: &str,
    expected: &[u8],
) -> io::Result<()>
where
    F: FnMut(&mut SupervisedCommand, Duration) -> io::Result<()>,
{
    let stdout = artifacts.join(format!("{label}.stdout.txt"));
    let stderr = artifacts.join(format!("{label}.stderr.txt"));
    command
        .stdout(CommandStream::File(output(&stdout)?))
        .stderr(CommandStream::File(output(&stderr)?));
    supervisor
        .run_owned(command, Duration::from_secs(15))
        .map_err(io::Error::other)?;
    check_command_output(artifacts, label, expected)
}

fn check_command_output(artifacts: &Path, label: &str, expected: &[u8]) -> io::Result<()> {
    let stdout = artifacts.join(format!("{label}.stdout.txt"));
    let stderr = artifacts.join(format!("{label}.stderr.txt"));
    if read(&stdout, 4096)? != expected || !read(&stderr, 4096)?.is_empty() {
        return Err(io::Error::other(
            "server command output changed its contract",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn dependencies<F>(
    supervisor: &mut RuntimeCommands<'_, F>,
    executable: &Path,
    artifacts: &Path,
) -> io::Result<()>
where
    F: FnMut(&mut SupervisedCommand, Duration) -> io::Result<()>,
{
    let mut command = match std::env::consts::OS {
        "macos" => {
            let mut command = SupervisedCommand::new("otool").map_err(io::Error::other)?;
            command.arg("-L").map_err(io::Error::other)?;
            command
        }
        "linux" => SupervisedCommand::new("ldd").map_err(io::Error::other)?,
        _ => {
            return Err(io::Error::other(
                "native dependency inspection is unsupported",
            ))
        }
    };
    let stdout = artifacts.join("dependencies.stdout.txt");
    let stderr = artifacts.join("dependencies.stderr.txt");
    command
        .arg(executable)
        .map_err(io::Error::other)?
        .stdout(CommandStream::File(output(&stdout)?))
        .stderr(CommandStream::File(output(&stderr)?));
    supervisor
        .run_owned(command, Duration::from_secs(15))
        .map_err(io::Error::other)?;
    let bytes = read(&stdout, 65_536)?;
    check_dependencies_output(&bytes, &read(&stderr, 4096)?, std::env::consts::OS)
}

fn check_dependencies_output(bytes: &[u8], diagnostic: &[u8], os: &str) -> io::Result<()> {
    let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    if !diagnostic.is_empty() || text.contains("not found") {
        return Err(io::Error::other("server dependency resolution failed"));
    }
    let entries = match os {
        "macos" => text.lines().skip(1).collect::<Vec<_>>(),
        "linux" => text.lines().collect::<Vec<_>>(),
        _ => {
            return Err(io::Error::other(
                "native dependency inspection is unsupported",
            ))
        }
    };
    if entries.is_empty() || entries.len() > 64 {
        return Err(io::Error::other("server dependency inventory is invalid"));
    }
    for entry in entries {
        let entry = entry.trim();
        let permitted = match os {
            "macos" => entry.starts_with("/usr/lib/") || entry.starts_with("/System/Library/"),
            "linux" => {
                let path = entry
                    .split_once("=>")
                    .map_or(entry, |(_, path)| path.trim());
                path.starts_with("/lib/")
                    || path.starts_with("/lib64/")
                    || path.starts_with("/usr/lib/")
                    || path.starts_with("linux-vdso.so.")
            }
            _ => false,
        };
        if !permitted {
            return Err(io::Error::other(
                "server resolves a dependency outside system directories",
            ));
        }
    }
    Ok(())
}

pub(crate) fn check_native_server_outputs(
    candidate: &ServerCandidate,
    artifacts: &Path,
    os: &str,
    http: SocketAddr,
    executable: &crate::recorded_path::RecordedPath,
) -> io::Result<()> {
    if read(&artifacts.join("SOURCE.sha256"), 65)?
        != format!("{}\n", candidate.source_digest()).as_bytes()
    {
        return Err(io::Error::other("native server output source differs"));
    }
    let stdout = read(&artifacts.join("dependencies.stdout.txt"), 65_536)?;
    let stderr = read(&artifacts.join("dependencies.stderr.txt"), 4096)?;
    if os == "windows" {
        super::windows_dependencies::check_output_for_path(&stdout, &stderr, executable)?;
    } else {
        check_dependencies_output(&stdout, &stderr, os)?;
    }
    check_command_output(
        artifacts,
        "version",
        format!("rusterisk {}\n", candidate.name().version()).as_bytes(),
    )?;
    for label in ["packaged-config", "runtime-config"] {
        check_command_output(artifacts, label, b"Configuration valid.\n")?;
    }
    if !read(&artifacts.join("server.stderr.txt"), 1024 * 1024)?.is_empty()
        || !check_server_ready_log(
            &read(&artifacts.join("server.stdout.txt"), 1024 * 1024)?,
            http,
        )?
    {
        return Err(io::Error::other("native server runtime output differs"));
    }
    Ok(())
}

#[cfg(windows)]
fn observe_windows_dependencies<F>(
    service: &mut RuntimeService<'_, F>,
    executable: &Path,
    artifacts: &Path,
) -> io::Result<()>
where
    F: FnMut(&mut SupervisedCommand, Duration) -> io::Result<()>,
{
    let executable = executable
        .to_str()
        .ok_or_else(|| io::Error::other("server executable path is not UTF-8"))?;
    let pid = service.process_id().map_err(io::Error::other)?;
    let mut command = super::windows_dependencies::command(pid)?;
    let stdout = artifacts.join("dependencies.stdout.txt");
    let stderr = artifacts.join("dependencies.stderr.txt");
    command
        .stdout(CommandStream::File(output(&stdout)?))
        .stderr(CommandStream::File(output(&stderr)?));
    service
        .run_owned(command, Duration::from_secs(15))
        .map_err(io::Error::other)?;
    super::windows_dependencies::check_output(
        &read(&stdout, 65_536)?,
        &read(&stderr, 4096)?,
        executable,
    )
}

fn startup<F>(
    candidate: &ServerCandidate,
    execution: &Path,
    artifacts: &Path,
    supervisor: &mut RuntimeCommands<'_, F>,
    client: impl FnOnce(&mut RuntimeService<'_, F>, SocketAddr) -> io::Result<()>,
) -> io::Result<()>
where
    F: FnMut(&mut SupervisedCommand, Duration) -> io::Result<()>,
{
    let executable = execution.join(candidate.name().target().binary_path());
    #[cfg(unix)]
    dependencies(supervisor, &executable, artifacts)?;
    let mut version = SupervisedCommand::new(&executable).map_err(io::Error::other)?;
    version.arg("--version").map_err(io::Error::other)?;
    check_command(
        supervisor,
        version,
        artifacts,
        "version",
        format!("rusterisk {}\n", candidate.name().version()).as_bytes(),
    )?;
    let mut packaged = SupervisedCommand::new(&executable).map_err(io::Error::other)?;
    packaged
        .arg("--check-config")
        .map_err(io::Error::other)?
        .arg("--config-dir")
        .map_err(io::Error::other)?
        .arg(execution.join("config"))
        .map_err(io::Error::other)?;
    check_command(
        supervisor,
        packaged,
        artifacts,
        "packaged-config",
        b"Configuration valid.\n",
    )?;
    let http_reservation = TcpListener::bind("127.0.0.1:0")?;
    let sip_reservation = UdpSocket::bind("127.0.0.1:0")?;
    let http = http_reservation.local_addr()?;
    let sip = sip_reservation.local_addr()?;
    let configuration = ServerRuntimeConfig::capture(http, sip, |path| {
        candidate
            .files()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| bytes.to_vec())
            .ok_or_else(|| io::Error::other("server sample configuration missing"))
    })?;
    let runtime = execution.join("runtime");
    std::fs::create_dir(&runtime)?;
    std::fs::create_dir(runtime.join("config"))?;
    for (path, bytes) in configuration.files() {
        write(&runtime.join(path), bytes)?;
    }
    let mut configured = SupervisedCommand::new(&executable).map_err(io::Error::other)?;
    configured
        .arg("--check-config")
        .map_err(io::Error::other)?
        .arg("--config-dir")
        .map_err(io::Error::other)?
        .arg(runtime.join("config"))
        .map_err(io::Error::other)?;
    check_command(
        supervisor,
        configured,
        artifacts,
        "runtime-config",
        b"Configuration valid.\n",
    )?;
    let log = artifacts.join("server.stdout.txt");
    let diagnostic = artifacts.join("server.stderr.txt");
    let mut command = SupervisedCommand::new(&executable).map_err(io::Error::other)?;
    command
        .arg("--config-dir")
        .map_err(io::Error::other)?
        .arg(runtime.join("config"))
        .map_err(io::Error::other)?
        .arg("--data-dir")
        .map_err(io::Error::other)?
        .arg(runtime.join("data"))
        .map_err(io::Error::other)?
        .current_dir(&runtime)
        .map_err(io::Error::other)?
        .stdout(CommandStream::File(output(&log)?))
        .stderr(CommandStream::File(output(&diagnostic)?));
    drop(http_reservation);
    drop(sip_reservation);
    let mut service = supervisor
        .start_service_owned(command, Duration::from_secs(90))
        .map_err(io::Error::other)?;
    let operation = (|| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            service.poll().map_err(io::Error::other)?;
            if check_server_ready_log(&read(&log, 1024 * 1024)?, http)? {
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other("server readiness deadline expired"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let result = client(&mut service, http);
        combine(
            result,
            service.poll().map_err(io::Error::other),
            "server lifetime",
        )?;
        #[cfg(windows)]
        observe_windows_dependencies(&mut service, &executable, artifacts)?;
        Ok(())
    })();
    let operation = combine(
        operation,
        service.stop().map_err(io::Error::other),
        "server shutdown",
    );
    combine(
        operation,
        (|| {
            if !read(&diagnostic, 1024 * 1024)?.is_empty()
                || !check_server_ready_log(&read(&log, 1024 * 1024)?, http)?
            {
                return Err(io::Error::other("server runtime diagnostics changed"));
            }
            let _http = TcpListener::bind(http)?;
            let _sip = UdpSocket::bind(sip)?;
            Ok(())
        })(),
        "server listener cleanup",
    )
}

pub fn verify_server_native_with(
    candidate: &Path,
    execution: &Path,
    artifacts: &Path,
    supervisor: &mut CommandSupervisor,
    client: impl FnOnce(&mut RunningCommand<'_>, SocketAddr) -> io::Result<()>,
) -> io::Result<()> {
    let candidate = ServerCandidate::read(candidate, native_target()?)?;
    verify_loaded_server_with(&candidate, execution, artifacts, supervisor, client)
}

pub(super) fn verify_loaded_server_with(
    candidate: &ServerCandidate,
    execution: &Path,
    artifacts: &Path,
    supervisor: &mut CommandSupervisor,
    client: impl FnOnce(&mut RunningCommand<'_>, SocketAddr) -> io::Result<()>,
) -> io::Result<()> {
    let mut configure = |_: &mut SupervisedCommand, _: Duration| Ok(());
    let mut commands = RuntimeCommands::new(supervisor, &mut configure);
    verify_loaded_server_configured(
        candidate,
        execution,
        artifacts,
        &mut commands,
        |service, address| client(service.running(), address),
    )
}

pub(crate) fn verify_loaded_server_configured<F>(
    candidate: &ServerCandidate,
    execution: &Path,
    artifacts: &Path,
    supervisor: &mut RuntimeCommands<'_, F>,
    client: impl FnOnce(&mut RuntimeService<'_, F>, SocketAddr) -> io::Result<()>,
) -> io::Result<()>
where
    F: FnMut(&mut SupervisedCommand, Duration) -> io::Result<()>,
{
    if !execution.is_absolute()
        || !artifacts.is_absolute()
        || execution.starts_with(artifacts)
        || artifacts.starts_with(execution)
        || [execution, artifacts].iter().any(|path| {
            path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
        })
    {
        return Err(io::Error::other(
            "server checks require separate absolute execution and artifact paths",
        ));
    }
    for path in [execution, artifacts] {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("server check parent missing"))?;
        if std::fs::canonicalize(parent)? != parent {
            return Err(io::Error::other(
                "server checks require physical parent directories",
            ));
        }
    }
    std::fs::create_dir(artifacts)?;
    write(
        &artifacts.join("SOURCE.sha256"),
        format!("{}\n", candidate.source_digest()).as_bytes(),
    )?;
    let extracted = candidate.extract(execution)?;
    let operation = startup(
        candidate,
        extracted.directory()?,
        artifacts,
        supervisor,
        client,
    );
    combine(operation, extracted.close(), "server extraction cleanup")
}

pub fn check_server_startup_with(
    root: &Path,
    candidate: &Path,
    supervisor: &mut CommandSupervisor,
) -> io::Result<std::path::PathBuf> {
    check_server_startup_for_target_with(root, candidate, native_target()?, false, supervisor)
}

pub fn check_server_startup_for_target_with(
    root: &Path,
    candidate: &Path,
    target: ServerTarget,
    translation: bool,
    supervisor: &mut CommandSupervisor,
) -> io::Result<std::path::PathBuf> {
    let mode = super::ServerExecutionMode::select(
        target,
        std::env::consts::OS,
        std::env::consts::ARCH,
        translation,
    )?;
    let candidate = ServerCandidate::read(candidate, target)?;
    let mut parent = std::fs::canonicalize(root)?;
    for component in [".cache", "distributions", "server-startup"] {
        parent.push(component);
        super::command::ensure_directory(&parent)?;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let identity = format!("{}-{stamp}", std::process::id());
    let artifacts = parent.join(&identity);
    let execution = std::fs::canonicalize(std::env::temp_dir())?
        .join(format!("rusterisk-server-startup-{identity}"));
    println!("server startup artifacts: {}", artifacts.display());
    println!(
        "server startup target={} execution={}",
        target.triple(),
        mode.label()
    );
    io::stdout().flush()?;
    verify_loaded_server_with(
        &candidate,
        &execution,
        &artifacts,
        supervisor,
        |_, _| Ok(()),
    )?;
    println!("server candidate startup passed");
    Ok(artifacts)
}
