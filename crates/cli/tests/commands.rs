use axum::{
    http::{HeaderMap, StatusCode},
    routing::{on, MethodFilter},
    Router,
};
use std::{
    io,
    process::{Output, Stdio},
    time::Duration,
};
use tokio::{io::AsyncWriteExt, net::TcpListener, process::Command};

async fn invoke(arguments: &[&str], password: &[u8]) -> io::Result<Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rusterisk-cli"))
        .args(arguments)
        .env_remove("REQUEST_METHOD")
        .env("HTTP_PROXY", "http://127.0.0.1:0")
        .env("NO_PROXY", "")
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("missing stdin"))?;
    input.write_all(password).await?;
    drop(input);
    tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await?
}

async fn exchange(
    command: &str,
    method: MethodFilter,
    path: &str,
    status: StatusCode,
    body: &'static str,
) -> io::Result<Output> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let router = Router::new().route(
        path,
        on(method, move |headers: HeaderMap| async move {
            if headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                == Some("Basic dGVzdDpzYW1wbGU=")
            {
                (status, body)
            } else {
                (StatusCode::UNAUTHORIZED, "")
            }
        }),
    );
    let arguments = [
        command,
        "--server",
        &origin,
        "--username",
        "test",
        "--password-stdin",
    ];
    tokio::select! {
        result = invoke(&arguments, b"sample\r\n") => result,
        result = axum::serve(listener, router) => Err(io::Error::other(format!("fixture stopped: {result:?}"))),
    }
}

#[tokio::test]
async fn loopback_commands_bypass_the_proxy_and_require_completed_responses() -> io::Result<()> {
    let ping = exchange(
        "ping",
        MethodFilter::GET,
        "/ari/asterisk/ping",
        StatusCode::OK,
        r#"{"ping":"pong","timestamp":"2026-11-20T00:00:00Z","asterisk_id":"telephone"}"#,
    )
    .await?;
    assert!(
        ping.status.success(),
        "{}",
        String::from_utf8_lossy(&ping.stderr)
    );
    assert_eq!(ping.stdout, b"pong\n");
    let malformed = exchange(
        "ping",
        MethodFilter::GET,
        "/ari/asterisk/ping",
        StatusCode::OK,
        "{}",
    )
    .await?;
    assert!(!malformed.status.success());
    let success = exchange(
        "reload",
        MethodFilter::POST,
        "/rusterisk/configuration/reload",
        StatusCode::NO_CONTENT,
        "",
    )
    .await?;
    assert!(
        success.status.success(),
        "{}",
        String::from_utf8_lossy(&success.stderr)
    );
    assert_eq!(success.stdout, b"Configuration reloaded.\n");
    let pending = exchange(
        "reload",
        MethodFilter::POST,
        "/rusterisk/configuration/reload",
        StatusCode::ACCEPTED,
        "",
    )
    .await?;
    assert_eq!(pending.status.code(), Some(1));
    assert!(pending.stdout.is_empty());
    assert!(String::from_utf8_lossy(&pending.stderr).contains("202"));
    Ok(())
}

#[tokio::test]
async fn command_line_help_and_input_failures_work_without_a_server() -> io::Result<()> {
    let subcommand_help = invoke(&["reload", "--help"], b"").await?;
    assert!(subcommand_help.status.success());
    assert!(String::from_utf8_lossy(&subcommand_help.stdout).contains("--server"));
    let help = invoke(&["--help"], b"").await?;
    assert!(help.status.success());
    let usage = String::from_utf8_lossy(&help.stdout);
    assert!(
        usage.contains("ping") && usage.contains("reload") && usage.contains("--password-stdin")
    );
    let version = invoke(&["--version"], b"").await?;
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        concat!("rusterisk-cli ", env!("CARGO_PKG_VERSION"))
    );
    let invalid = invoke(
        &[
            "ping",
            "--server",
            "http://127.0.0.1:9",
            "--username",
            "test",
            "--password-stdin",
        ],
        b"sample\nextra\n",
    )
    .await?;
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains("sample"));
    Ok(())
}
