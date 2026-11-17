#![cfg(unix)]

use base64::Engine;
use reqwest::{Client, StatusCode};
use std::{
    fs::{self, File},
    io::{self, Read},
    net::{SocketAddr, TcpListener, UdpSocket},
    path::{Path, PathBuf},
    process::{ExitStatus, Output, Stdio},
    time::Duration,
};
use tokio::process::{Child, Command};

const CURRENT: &str = "process-current-secret";
const REPLACEMENT: &str = "process-replacement-secret";
const INVALID_CONFIG: &[u8] = b"[general]\nunsupported_client_setting=yes\n";

fn verify(condition: bool, message: impl std::fmt::Display) -> io::Result<()> {
    condition
        .then_some(())
        .ok_or_else(|| io::Error::other(message.to_string()))
}

fn combine<T>(primary: io::Result<T>, cleanup: io::Result<()>) -> io::Result<T> {
    match (primary, cleanup) {
        (primary, Ok(())) => primary,
        (Ok(_), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
            "{primary}; configuration reload process cleanup failed: {cleanup}"
        ))),
    }
}

fn output(directory: &Path, name: &str) -> io::Result<Stdio> {
    File::create_new(directory.join(name)).map(Stdio::from)
}

fn read(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    verify(
        bytes.len() as u64 <= limit,
        "Process diagnostics exceeded capacity",
    )?;
    Ok(bytes)
}

fn redacted(bytes: &[u8]) -> io::Result<()> {
    for password in [CURRENT, REPLACEMENT] {
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(format!("operator:{password}"));
        for secret in [password.as_bytes(), encoded.as_bytes()] {
            verify(
                !bytes.windows(secret.len()).any(|window| window == secret),
                "Process output disclosed management credentials",
            )?;
        }
    }
    Ok(())
}

fn isolate_proxy(command: &mut Command) {
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        command.env_remove(name);
    }
    command.env("NO_PROXY", "127.0.0.1");
}

async fn waited(child: &mut Child, budget: Duration) -> io::Result<ExitStatus> {
    tokio::time::timeout(budget, child.wait())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Process deadline elapsed"))?
}

async fn forced(child: &mut Child) -> io::Result<()> {
    let killed = match child.try_wait() {
        Ok(Some(_)) => Ok(()),
        Ok(None) => child.start_kill(),
        Err(primary) => combine(Err(primary), child.start_kill()),
    };
    combine(
        killed,
        waited(child, Duration::from_secs(2)).await.map(|_| ()),
    )
}

async fn completed(child: &mut Child, budget: Duration) -> io::Result<ExitStatus> {
    match waited(child, budget).await {
        Ok(status) => Ok(status),
        Err(primary) => combine(Err(primary), forced(child).await),
    }
}

async fn terminate(pid: u32) -> io::Result<()> {
    let mut signal = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let status = completed(&mut signal, Duration::from_secs(2)).await?;
    verify(
        status.success(),
        format!("Termination request failed: {status}"),
    )
}

async fn shutdown(child: &mut Child) -> io::Result<()> {
    let primary = async {
        verify(
            child.try_wait()?.is_none(),
            "Server exited before graceful shutdown",
        )?;
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("Server process identity was released"))?;
        terminate(pid).await?;
        let status = waited(child, Duration::from_secs(5)).await?;
        verify(
            status.success(),
            format!("Server did not shut down gracefully: {status}"),
        )
    }
    .await;
    if primary.is_err() {
        combine(primary, forced(child).await)
    } else {
        primary
    }
}

fn credentials(settings: &Path, password: &str) -> io::Result<()> {
    fs::write(
        settings.join("ari.conf"),
        format!("[operator]\ntype=user\npassword={password}\n"),
    )
}

struct Fixture {
    child: Child,
    client: Client,
    address: SocketAddr,
    sip: SocketAddr,
    root: PathBuf,
    settings: PathBuf,
    data: PathBuf,
    client_root: PathBuf,
}

impl Fixture {
    fn start(root: &Path) -> io::Result<Self> {
        let root = root.canonicalize()?;
        let settings = root.join("config");
        let data = root.join("data");
        let client_root = root.join("client with invalid config");
        fs::create_dir(&settings)?;
        fs::create_dir(&client_root)?;
        fs::create_dir(client_root.join("config"))?;
        fs::write(client_root.join("config/http.conf"), INVALID_CONFIG)?;
        fs::write(client_root.join("password-stdin"), format!("{CURRENT}\n"))?;
        let http = TcpListener::bind("127.0.0.1:0")?;
        let udp = UdpSocket::bind("127.0.0.1:0")?;
        let address = http.local_addr()?;
        let sip = udp.local_addr()?;
        for (name, text) in [
            (
                "http.conf",
                format!(
                    "[general]\nbindaddr=127.0.0.1\nbindport={}\n",
                    address.port()
                ),
            ),
            (
                "pjsip.conf",
                format!("[udp]\ntype=transport\nprotocol=udp\nbind={sip}\n"),
            ),
            ("rtp.conf", String::new()),
            ("extensions.conf", String::new()),
        ] {
            fs::write(settings.join(name), text)?;
        }
        credentials(&settings, CURRENT)?;
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .connect_timeout(Duration::from_secs(2))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(io::Error::other)?;
        let stdout = output(&root, "server.stdout")?;
        let stderr = output(&root, "server.stderr")?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_rusterisk"));
        command
            .current_dir(&root)
            .arg("--config-dir")
            .arg(&settings)
            .arg("--data-dir")
            .arg(&data)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .kill_on_drop(true);
        isolate_proxy(&mut command);
        drop(http);
        drop(udp);
        let child = command.spawn()?;
        Ok(Self {
            child,
            client,
            address,
            sip,
            root,
            settings,
            data,
            client_root,
        })
    }

    async fn ping(&self, password: &str) -> io::Result<StatusCode> {
        let mut response = self
            .client
            .get(format!("http://{}/ari/asterisk/ping", self.address))
            .basic_auth("operator", Some(password))
            .send()
            .await
            .map_err(|error| {
                let kind = if error.is_connect() {
                    io::ErrorKind::NotConnected
                } else {
                    io::ErrorKind::Other
                };
                io::Error::new(kind, error.without_url())
            })?;
        let status = response.status();
        let mut size = 0usize;
        while let Some(bytes) = response.chunk().await.map_err(io::Error::other)? {
            size = size
                .checked_add(bytes.len())
                .filter(|size| *size <= 4096)
                .ok_or_else(|| io::Error::other("HTTP ping response exceeded capacity"))?;
        }
        Ok(status)
    }

    async fn ready(&mut self) -> io::Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = self.child.try_wait()? {
                    return Err(io::Error::other(format!(
                        "Server exited before HTTP readiness: {status}"
                    )));
                }
                match self.ping(CURRENT).await {
                    Ok(StatusCode::OK) => return Ok(()),
                    Ok(status) => {
                        return Err(io::Error::other(format!(
                            "Server readiness returned HTTP {status}"
                        )))
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotConnected => {}
                    Err(error) => return Err(error),
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "Server HTTP readiness deadline elapsed",
            )
        })??;
        verify(
            matches!(
                rusterisk_storage::Store::open(&self.data),
                Err(rusterisk_storage::StorageError::InUse)
            ),
            "Running server did not own its Store",
        )
    }

    async fn reload(&self) -> io::Result<Output> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rusterisk"));
        command
            .current_dir(&self.client_root)
            .args([
                "reload",
                "--server",
                &format!("http://{}", self.address),
                "--username",
                "operator",
                "--password-stdin",
            ])
            .stdin(Stdio::from(File::open(
                self.client_root.join("password-stdin"),
            )?))
            .stdout(output(&self.client_root, "stdout")?)
            .stderr(output(&self.client_root, "stderr")?)
            .kill_on_drop(true);
        isolate_proxy(&mut command);
        let mut child = command.spawn()?;
        let status = completed(&mut child, Duration::from_secs(5)).await?;
        Ok(Output {
            status,
            stdout: read(&self.client_root.join("stdout"), 8192)?,
            stderr: read(&self.client_root.join("stderr"), 8192)?,
        })
    }

    async fn publish(&mut self) -> io::Result<()> {
        self.ready().await?;
        credentials(&self.settings, REPLACEMENT)?;
        let result = self.reload().await?;
        redacted(&result.stdout)?;
        redacted(&result.stderr)?;
        verify(
            result.status.success(),
            format!(
                "Reload failed with {}: {}",
                result.status,
                String::from_utf8_lossy(&result.stderr)
            ),
        )?;
        verify(
            result.stdout == b"Configuration reloaded.\n" && result.stderr.is_empty(),
            "Reload did not confirm applied configuration",
        )?;
        verify(
            self.ping(REPLACEMENT).await? == StatusCode::OK,
            "New management credentials were not published",
        )?;
        verify(
            self.ping(CURRENT).await? == StatusCode::UNAUTHORIZED,
            "Old management credentials remained authorized",
        )
    }

    async fn finish(self, primary: io::Result<()>) -> io::Result<()> {
        let Self {
            mut child,
            client,
            address,
            sip,
            root,
            settings: _,
            data,
            client_root,
        } = self;
        drop(client);
        let mut result = combine(primary, shutdown(&mut child).await);
        for cleanup in [
            TcpListener::bind(address).map(drop),
            UdpSocket::bind(sip).map(drop),
            rusterisk_storage::Store::open(&data)
                .map(drop)
                .map_err(io::Error::other),
            unchanged(&client_root),
        ] {
            result = combine(result, cleanup);
        }
        for path in [root.join("server.stdout"), root.join("server.stderr")] {
            result = combine(
                result,
                read(&path, 65_536).and_then(|bytes| redacted(&bytes)),
            );
        }
        result
    }
}

fn unchanged(directory: &Path) -> io::Result<()> {
    verify(
        fs::read(directory.join("config/http.conf"))? == INVALID_CONFIG,
        "Reload client changed its unrelated local configuration",
    )?;
    match fs::symlink_metadata(directory.join("data")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::other(
            "Reload client started a local server Store",
        )),
    }
}

#[tokio::test]
async fn a_running_binary_reloads_its_starting_directory_and_releases_owned_resources(
) -> io::Result<()> {
    let directory = tempfile::Builder::new()
        .prefix("configuration reload with spaces ")
        .tempdir()?;
    let primary = async {
        let mut fixture = Fixture::start(directory.path())?;
        let primary = fixture.publish().await;
        fixture.finish(primary).await
    }
    .await;
    combine(primary, directory.close())
}
