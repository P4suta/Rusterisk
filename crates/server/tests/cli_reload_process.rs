use axum::{http::StatusCode, response::IntoResponse};
use base64::Engine;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    net::{SocketAddr, TcpListener},
    path::Path,
    process::{ExitStatus, Output, Stdio},
    time::Duration,
};
use tokio::{
    process::{Child, Command},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

const INVALID_CONFIG: &[u8] = b"[general]\nunsupported_reload_setting=yes\n";
const APPLIED: &[u8] = b"Configuration reloaded.\n";

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
            "{primary}; reload process fixture cleanup failed: {cleanup}"
        ))),
    }
}

fn prepare(directory: &Path, input: &[u8]) -> io::Result<()> {
    fs::create_dir(directory.join("config"))?;
    fs::write(directory.join("config/http.conf"), INVALID_CONFIG)?;
    fs::write(directory.join("password-stdin"), input)
}

fn unchanged(directory: &Path) -> io::Result<()> {
    verify(
        fs::read(directory.join("config/http.conf"))? == INVALID_CONFIG,
        "Reload changed local configuration",
    )?;
    for name in [
        "data",
        ".rusterisk.lock",
        "calls.sqlite3",
        "calls.sqlite3-wal",
        "calls.sqlite3-shm",
        "recordings",
    ] {
        match fs::symlink_metadata(directory.join(name)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
            Ok(_) => return Err(io::Error::other("Reload created local server state")),
        }
    }
    Ok(())
}

fn read(directory: &Path, name: &str) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(directory.join(name))?
        .take(8193)
        .read_to_end(&mut bytes)?;
    verify(bytes.len() <= 8192, "Reload output exceeded capacity")?;
    Ok(bytes)
}

async fn stop(child: &mut Child) -> io::Result<()> {
    let killed = match child.try_wait() {
        Ok(Some(_)) => Ok(()),
        Ok(None) => child.start_kill(),
        Err(primary) => combine(Err(primary), child.start_kill()),
    };
    let reaped = tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Reload child was not reaped"))
        .and_then(|result| result)
        .map(|_| ());
    combine(killed, reaped)
}

async fn wait(child: &mut Child) -> io::Result<ExitStatus> {
    let result = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "Reload child exceeded five seconds",
            )
        })
        .and_then(|result| result);
    match result {
        Ok(status) => Ok(status),
        Err(error) => combine(Err(error), stop(child).await),
    }
}

async fn run(directory: &Path, address: SocketAddr) -> io::Result<Output> {
    let stdout = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("stdout"))?;
    let stderr = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("stderr"))?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_rusterisk"));
    command
        .current_dir(directory)
        .args([
            "reload",
            "--server",
            &format!("http://{address}"),
            "--username",
            "operator",
            "--password-stdin",
        ])
        .stdin(Stdio::from(File::open(directory.join("password-stdin"))?))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true);
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
    let mut child = command.spawn()?;
    let status = wait(&mut child).await?;
    Ok(Output {
        status,
        stdout: read(directory, "stdout")?,
        stderr: read(directory, "stderr")?,
    })
}

#[derive(Clone, Copy)]
struct Reply {
    status: StatusCode,
    message: Option<&'static str>,
}

struct Snapshot {
    metadata: bool,
    authorized: bool,
    empty_body: bool,
}

struct HttpFixture {
    address: SocketAddr,
    lifetime: oneshot::Receiver<()>,
    server: JoinHandle<io::Result<()>>,
    observations: mpsc::Receiver<Snapshot>,
}

impl HttpFixture {
    fn start(reply: Reply, password: &str) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let authorization = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("operator:{password}"))
        );
        let (sender, observations) = mpsc::channel(2);
        let app = axum::Router::new().fallback(axum::routing::any(
            move |request: axum::extract::Request| {
                let sender = sender.clone();
                let authorization = authorization.clone();
                async move {
                    let metadata =
                        rusterisk_api::ManagementOperation::ReloadConfiguration.metadata();
                    let (parts, body) = request.into_parts();
                    let snapshot = Snapshot {
                        metadata: parts.method.as_str() == metadata.method.upper()
                            && parts.uri.path() == metadata.path
                            && parts.uri.query().is_none(),
                        authorized: parts
                            .headers
                            .get(axum::http::header::AUTHORIZATION)
                            .is_some_and(|value| value == authorization.as_str()),
                        empty_body: axum::body::to_bytes(body, 0).await.is_ok(),
                    };
                    sender
                        .try_send(snapshot)
                        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
                    Ok::<_, StatusCode>(match reply.message {
                        Some(message) => (
                            reply.status,
                            axum::Json(rusterisk_api::ApiError {
                                message: message.to_owned(),
                            }),
                        )
                            .into_response(),
                        None => reply.status.into_response(),
                    })
                }
            },
        ));
        let (mut stop, lifetime) = oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { stop.closed().await })
                .await
        });
        Ok(Self {
            address,
            lifetime,
            server,
            observations,
        })
    }

    async fn finish(self, primary: io::Result<Output>) -> io::Result<Output> {
        let Self {
            address,
            lifetime,
            mut server,
            mut observations,
        } = self;
        drop(lifetime);
        let joined = match tokio::time::timeout(Duration::from_secs(2), &mut server).await {
            Ok(result) => result.map_err(io::Error::other).and_then(|result| result),
            Err(_) => {
                server.abort();
                let reaped = match server.await {
                    Err(error) if error.is_cancelled() => Ok(()),
                    result => result.map_err(io::Error::other).and_then(|result| result),
                };
                combine(
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Reload HTTP fixture did not stop",
                    )),
                    reaped,
                )
            }
        };
        let checked = (|| {
            let snapshot = observations.try_recv().map_err(io::Error::other)?;
            verify(
                snapshot.metadata,
                "Reload used unexpected management metadata",
            )?;
            verify(
                snapshot.authorized,
                "Reload changed the supplied credentials",
            )?;
            verify(
                snapshot.empty_body,
                "Reload sent a replacement request body",
            )?;
            verify(
                matches!(
                    observations.try_recv(),
                    Err(mpsc::error::TryRecvError::Disconnected)
                ),
                "Reload issued more than one management request",
            )
        })();
        let primary = combine(combine(primary, joined), checked);
        combine(primary, TcpListener::bind(address).map(drop))
    }
}

fn redacted(output: &Output, secret: &[u8]) -> io::Result<()> {
    let mut credentials = b"operator:".to_vec();
    credentials.extend_from_slice(secret);
    let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
    for bytes in [&output.stdout, &output.stderr] {
        verify(
            secret.is_empty() || !bytes.windows(secret.len()).any(|window| window == secret),
            "Reload output disclosed the supplied password",
        )?;
        verify(
            !bytes
                .windows(encoded.len())
                .any(|window| window == encoded.as_bytes()),
            "Reload output disclosed encoded credentials",
        )?;
        for marker in [b"Basic ".as_slice(), b"Authorization".as_slice()] {
            verify(
                !bytes.windows(marker.len()).any(|window| window == marker),
                "Reload output disclosed an authorization header",
            )?;
        }
    }
    Ok(())
}

async fn exchange(reply: Reply, password: &str, suffix: &str) -> io::Result<Output> {
    let directory = tempfile::Builder::new()
        .prefix("reload process with spaces ")
        .tempdir()?;
    let primary = async {
        prepare(directory.path(), format!("{password}{suffix}").as_bytes())?;
        let fixture = HttpFixture::start(reply, password)?;
        let output = run(directory.path(), fixture.address).await;
        let output = fixture.finish(output).await?;
        redacted(&output, password.as_bytes())?;
        Ok(output)
    }
    .await;
    let primary = combine(primary, unchanged(directory.path()));
    combine(primary, directory.close())
}

#[tokio::test]
async fn only_applied_reload_succeeds_with_stdin_credentials_and_no_local_settings(
) -> io::Result<()> {
    let maximum = format!(" {} ", "é".repeat(127));
    verify(maximum.len() == 256, "The UTF-8 password boundary changed")?;
    for (password, suffix) in [
        (" current secret with spaces ", ""),
        (" current secret with spaces ", "\n"),
        (maximum.as_str(), "\r\n"),
    ] {
        let output = exchange(
            Reply {
                status: StatusCode::NO_CONTENT,
                message: None,
            },
            password,
            suffix,
        )
        .await?;
        verify(
            output.status.success(),
            "Applied reload did not exit successfully",
        )?;
        verify(
            output.stdout == APPLIED,
            "Applied reload confirmation changed",
        )?;
        verify(output.stderr.is_empty(), "Applied reload wrote an error")?;
    }
    Ok(())
}

#[tokio::test]
async fn pending_and_rejected_reload_preserve_status_and_position_without_exposing_credentials(
) -> io::Result<()> {
    for reply in [
        Reply {
            status: StatusCode::ACCEPTED,
            message: None,
        },
        Reply {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: Some("pjsip.conf:9: missing endpoint auth"),
        },
    ] {
        let output = exchange(reply, "stdin-secret-for-reload", "\n").await?;
        verify(
            !output.status.success(),
            "Unapplied reload exited successfully",
        )?;
        verify(
            output.stdout.is_empty(),
            "Unapplied reload reported success",
        )?;
        let diagnostic = String::from_utf8(output.stderr).map_err(io::Error::other)?;
        verify(
            diagnostic.contains(&reply.status.as_u16().to_string()),
            "Reload omitted the remote failure status",
        )?;
        if let Some(message) = reply.message {
            verify(
                diagnostic.contains(message),
                "Reload lost the file, line, or reason",
            )?;
        }
    }
    Ok(())
}

async fn rejected_input(input: &[u8], secret: &[u8]) -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let primary = async {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let primary = async {
            prepare(directory.path(), input)?;
            let output = run(directory.path(), address).await?;
            redacted(&output, secret)?;
            verify(
                output.status.code() == Some(2),
                "Invalid stdin did not exit with status 2",
            )?;
            verify(
                output.stdout.is_empty(),
                "Invalid stdin reported applied configuration",
            )?;
            verify(
                !output.stderr.is_empty(),
                "Invalid stdin omitted its input diagnostic",
            )
        }
        .await;
        let unconnected = match listener.accept() {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(error) => Err(error),
            Ok((stream, _)) => {
                drop(stream);
                Err(io::Error::other(
                    "Invalid stdin connected to the live server",
                ))
            }
        };
        drop(listener);
        let primary = combine(primary, unconnected);
        combine(primary, TcpListener::bind(address).map(drop))
    }
    .await;
    let primary = combine(primary, unchanged(directory.path()));
    combine(primary, directory.close())
}

#[tokio::test]
async fn invalid_stdin_is_rejected_before_connecting_to_the_server() -> io::Result<()> {
    let oversized = "s".repeat(257);
    let oversized_utf8 = format!("{}x", "é".repeat(128));
    for (input, secret) in [
        (b"".as_slice(), b"".as_slice()),
        (b"\n".as_slice(), b"".as_slice()),
        (oversized.as_bytes(), oversized.as_bytes()),
        (oversized_utf8.as_bytes(), oversized_utf8.as_bytes()),
        (b"stdin-secret\xff\n".as_slice(), b"stdin-secret".as_slice()),
        (b"stdin-secret\n\n".as_slice(), b"stdin-secret".as_slice()),
        (
            b"stdin-secret\r\n\r\n".as_slice(),
            b"stdin-secret".as_slice(),
        ),
    ] {
        rejected_input(input, secret).await?;
    }
    Ok(())
}
