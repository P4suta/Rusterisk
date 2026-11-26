use rusterisk_server::ControlError;
use rusterisk_test_support::Wait;
use std::{
    fs::File,
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
};

pub fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

pub fn combine(
    primary: Result<(), ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<(), ControlError> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(failure(format!(
            "{primary}; process cleanup failed: {cleanup}"
        ))),
    }
}

#[derive(serde::Deserialize)]
struct Log {
    fields: Fields,
}

#[derive(serde::Deserialize)]
struct Fields {
    message: String,
    http: Option<SocketAddr>,
}

pub struct ChildServer {
    child: Child,
    address: SocketAddr,
}

async fn terminate(child: &mut Child) -> Result<ExitStatus, ControlError> {
    if let Some(status) = child.try_wait().map_err(failure)? {
        return Ok(status);
    }
    if let Err(primary) = child.kill() {
        return match child.try_wait().map_err(failure)? {
            Some(status) => Ok(status),
            None => Err(failure(primary)),
        };
    }
    rusterisk_test_support::child_exit(child, Wait::Ready).map_err(failure)
}

fn ready_line(path: &Path) -> Result<Option<SocketAddr>, ControlError> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(failure)?
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(failure)?;
    if bytes.len() > 65536 {
        return Err(ControlError::Capacity);
    }
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if line.last() != Some(&b'\n') {
            continue;
        }
        let log: Log = serde_json::from_slice(line)?;
        if log.fields.message == "server ready" {
            let address = log.fields.http.ok_or(ControlError::Capacity)?;
            if !address.ip().is_loopback() || address.port() == 0 {
                return Err(ControlError::Adapter("invalid process listener".into()));
            }
            return Ok(Some(address));
        }
    }
    Ok(None)
}

impl ChildServer {
    pub async fn start(settings: &Path, data: &Path, log: PathBuf) -> Result<Self, ControlError> {
        let mut last = None;
        for attempt in 0..4 {
            let reservation = std::net::TcpListener::bind("127.0.0.1:0").map_err(failure)?;
            let address = reservation.local_addr().map_err(failure)?;
            std::fs::write(
                settings.join("http.conf"),
                format!(
                    "[general]\nbindaddr=127.0.0.1\nbindport={}\n",
                    address.port()
                ),
            )
            .map_err(failure)?;
            drop(reservation);
            match Self::spawn(settings, data, log.with_extension(format!("{attempt}.log"))).await {
                Ok(server) => return Ok(server),
                Err(primary) => {
                    let occupied = match std::net::TcpListener::bind(address) {
                        Ok(listener) => {
                            drop(listener);
                            false
                        }
                        Err(error) => error.kind() == std::io::ErrorKind::AddrInUse,
                    };
                    if !occupied {
                        return Err(primary);
                    }
                    last = Some(primary);
                }
            }
        }
        Err(last.unwrap_or(ControlError::Capacity))
    }

    async fn spawn(settings: &Path, data: &Path, log: PathBuf) -> Result<Self, ControlError> {
        let output = File::create_new(&log).map_err(failure)?;
        let errors = File::create_new(log.with_extension("stderr")).map_err(failure)?;
        let mut child = Command::new(env!("CARGO_BIN_EXE_rusterisk"))
            .arg("--config-dir")
            .arg(settings)
            .arg("--data-dir")
            .arg(data)
            .stdin(Stdio::null())
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(errors))
            .spawn()
            .map_err(failure)?;
        let mut changes = rusterisk_test_support::DirectoryChanges::new(&log).map_err(failure)?;
        let ready = Wait::Ready
            .bounded(async {
                loop {
                    if let Some(status) = child.try_wait().map_err(failure)? {
                        return Err(failure(format!("server exited before readiness: {status}")));
                    }
                    if let Some(address) = ready_line(&log)? {
                        return Ok(address);
                    }
                    changes.changed().await.map_err(failure)?;
                }
            })
            .await
            .map_err(failure)
            .and_then(|result| result);
        match ready {
            Ok(address) => Ok(Self { child, address }),
            Err(primary) => match terminate(&mut child).await {
                Ok(_) => Err(primary),
                Err(cleanup) => Err(failure(format!(
                    "{primary}; startup cleanup failed: {cleanup}"
                ))),
            },
        }
    }

    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    pub async fn terminate(&mut self) -> Result<ExitStatus, ControlError> {
        terminate(&mut self.child).await
    }
}
