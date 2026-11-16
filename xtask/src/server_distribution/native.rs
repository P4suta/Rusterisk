use super::{check_server_ready_log, ServerCandidate, ServerRuntimeConfig, ServerTarget};
use crate::process::{CommandSupervisor, RunningCommand};
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, UdpSocket},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn native_target() -> io::Result<ServerTarget> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok(ServerTarget::MacOsArm64),
        ("macos", "x86_64") => Ok(ServerTarget::MacOsX86_64),
        ("linux", "aarch64") => Ok(ServerTarget::LinuxArm64),
        ("linux", "x86_64") => Ok(ServerTarget::LinuxX86_64),
        _ => Err(io::Error::other(
            "server startup requires a supported native host",
        )),
    }
}

fn read(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
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

fn output(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = output(path)?;
    file.write_all(bytes)?;
    file.flush()
}

fn combine(operation: io::Result<()>, cleanup: io::Result<()>, context: &str) -> io::Result<()> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; {context}: {cleanup}"
        ))),
    }
}

fn check_command(
    supervisor: &mut CommandSupervisor,
    command: &mut Command,
    artifacts: &Path,
    label: &str,
    expected: &[u8],
) -> io::Result<()> {
    let stdout = artifacts.join(format!("{label}.stdout.txt"));
    let stderr = artifacts.join(format!("{label}.stderr.txt"));
    command.stdout(output(&stdout)?).stderr(output(&stderr)?);
    supervisor
        .run(command, Duration::from_secs(15))
        .map_err(io::Error::other)?;
    if read(&stdout, 4096)? != expected || !read(&stderr, 4096)?.is_empty() {
        return Err(io::Error::other(
            "server command output changed its contract",
        ));
    }
    Ok(())
}

fn dependencies(
    supervisor: &mut CommandSupervisor,
    executable: &Path,
    artifacts: &Path,
) -> io::Result<()> {
    let mut command = match std::env::consts::OS {
        "macos" => {
            let mut command = Command::new("otool");
            command.arg("-L");
            command
        }
        "linux" => Command::new("ldd"),
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
        .stdout(output(&stdout)?)
        .stderr(output(&stderr)?);
    supervisor
        .run(&mut command, Duration::from_secs(15))
        .map_err(io::Error::other)?;
    let bytes = read(&stdout, 65_536)?;
    let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    if !read(&stderr, 4096)?.is_empty() || text.contains("not found") {
        return Err(io::Error::other("server dependency resolution failed"));
    }
    let entries = match std::env::consts::OS {
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
        let permitted = match std::env::consts::OS {
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

fn startup(
    candidate: &ServerCandidate,
    execution: &Path,
    artifacts: &Path,
    supervisor: &mut CommandSupervisor,
    client: impl FnOnce(&mut RunningCommand<'_>, SocketAddr) -> io::Result<()>,
) -> io::Result<()> {
    let executable = execution.join(candidate.name().target().binary_path());
    dependencies(supervisor, &executable, artifacts)?;
    check_command(
        supervisor,
        Command::new(&executable).arg("--version"),
        artifacts,
        "version",
        format!("rusterisk {}\n", candidate.name().version()).as_bytes(),
    )?;
    check_command(
        supervisor,
        Command::new(&executable)
            .arg("--check-config")
            .arg("--config-dir")
            .arg(execution.join("config")),
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
    check_command(
        supervisor,
        Command::new(&executable)
            .arg("--check-config")
            .arg("--config-dir")
            .arg(runtime.join("config")),
        artifacts,
        "runtime-config",
        b"Configuration valid.\n",
    )?;
    let log = artifacts.join("server.stdout.txt");
    let diagnostic = artifacts.join("server.stderr.txt");
    let mut command = Command::new(executable);
    command
        .arg("--config-dir")
        .arg(runtime.join("config"))
        .arg("--data-dir")
        .arg(runtime.join("data"))
        .current_dir(&runtime)
        .stdin(Stdio::null())
        .stdout(output(&log)?)
        .stderr(output(&diagnostic)?);
    drop(http_reservation);
    drop(sip_reservation);
    let mut service = supervisor
        .start_service(&mut command, Duration::from_secs(90))
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
        client(&mut service, http)
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
        &candidate,
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
    ServerCandidate::read(candidate, native_target()?)?;
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
    io::stdout().flush()?;
    verify_server_native_with(candidate, &execution, &artifacts, supervisor, |_, _| Ok(()))?;
    println!("server candidate startup passed");
    Ok(artifacts)
}
