use axum::{http::HeaderMap, routing::get, Router};
use serde_json::Value;
use std::{
    io,
    process::{Output, Stdio},
    time::Duration,
};
use tokio::{net::TcpListener, process::Command};

mod support;

async fn run(body: &'static str, json: bool) -> io::Result<Output> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let app = Router::new().route(
        "/rusterisk/observe/channels/test",
        get(move |headers: HeaderMap| async move {
            assert_eq!(
                headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                Some("Basic cmVhZGVyOnNlY3JldA==")
            );
            (
                [(axum::http::header::CONTENT_TYPE, "application/x-ndjson")],
                body,
            )
        }),
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_rusterisk-cli"));
    command.args([
        "watch",
        "channel",
        "test",
        "--server",
        &origin,
        "--username",
        "reader",
        "--password-stdin",
    ]);
    if json {
        command.arg("--json");
    }
    let child = command
        .stdin(support::password_input(b"secret\n")?)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    tokio::select! {
        out=tokio::time::timeout(Duration::from_secs(5),child.wait_with_output())=>out?,
        stopped = axum::serve(listener,app)=>Err(io::Error::other(format!("server stopped: {stopped:?}"))),
    }
}

const RECORDS:&str=concat!(
    "{\"type\":\"snapshot\",\"timestamp\":\"2026-11-21T06:00:00Z\",\"channels\":[{\"id\":\"test\",\"generation\":2,\"state\":\"Up\",\"endpoint\":\"desk\",\"unexpected\":\"secret\"}],\"endpoint\":null}\n",
    "{\"type\":\"event\",\"timestamp\":\"2026-11-21T06:00:01Z\",\"event\":\"ChannelDestroyed\",\"channel\":{\"id\":\"test\",\"generation\":2,\"state\":\"Down\",\"endpoint\":\"desk\"},\"bridge_id\":null,\"endpoint\":null,\"channelvars\":{\"PRIVATE\":\"secret\"}}\n"
);
#[tokio::test]
async fn a_complete_watch_flushes_safe_records_and_an_interrupted_stream_reports_a_gap(
) -> io::Result<()> {
    let output = run(RECORDS, true).await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = String::from_utf8(output.stdout).map_err(io::Error::other)?;
    let rows = bytes
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["type"], "snapshot");
    assert_eq!(rows[1]["event"], "ChannelDestroyed");
    assert!(!bytes.contains("secret") && !bytes.contains("PRIVATE"));
    let partial=run("{\"type\":\"snapshot\",\"timestamp\":\"2026-11-21T06:00:00Z\",\"channels\":[{\"id\":\"test\",\"generation\":2,\"state\":\"Up\",\"endpoint\":\"desk\"}],\"endpoint\":null}\n",true).await?;
    assert!(!partial.status.success());
    let text = String::from_utf8(partial.stdout).map_err(io::Error::other)?;
    let gap: Value = serde_json::from_str(
        text.lines()
            .last()
            .ok_or_else(|| io::Error::other("missing gap"))?,
    )?;
    assert_eq!(gap["type"], "gap");
    let terminal = run(RECORDS, false).await?;
    assert!(terminal.status.success());
    assert!(String::from_utf8_lossy(&terminal.stdout).contains("ChannelDestroyed"));
    Ok(())
}
