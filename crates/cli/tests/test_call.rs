#[path = "support/time.rs"]
pub mod test_time;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    io,
    process::{Output, Stdio},
};
use tokio::{net::TcpListener, process::Command};

mod support;

async fn endpoint(
    State(result): State<Value>,
    headers: HeaderMap,
    uri: Uri,
) -> (StatusCode, Json<Value>) {
    assert_eq!(headers["authorization"], "Basic dGVzdDpzZWNyZXQ=");
    assert_eq!(uri.query(), Some("deadline_seconds=3"));
    (StatusCode::OK, Json(result))
}
async fn probe(outcome: &str, answered: bool, verified: bool) -> io::Result<Output> {
    let result = json!({"channel_id":"probe-test","accepted":true,"answered":answered,"signal_verified":verified,"outcome":outcome});
    let app = Router::new()
        .route("/rusterisk/test-calls/PJSIP/echo", post(endpoint))
        .with_state(result);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let child = Command::new(env!("CARGO_BIN_EXE_rusterisk-cli"))
        .args([
            "test-call",
            "PJSIP/echo",
            "--deadline",
            "3",
            "--json",
            "--server",
            &format!("http://{}", listener.local_addr()?),
            "--username",
            "test",
            "--password-stdin",
        ])
        .stdin(support::password_input(b"secret\n")?)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    tokio::select! {out=crate::test_time::Wait::Ready.bounded(child.wait_with_output())=>out?,stopped=axum::serve(listener,app)=>Err(io::Error::other(format!("fixture stopped: {stopped:?}")))}
}
#[tokio::test]
async fn probe_reports_only_verified_received_audio_as_success() -> io::Result<()> {
    for (outcome, answered, verified) in [
        ("verified", true, true),
        ("busy", false, false),
        ("no_answer", false, false),
        ("silent", true, false),
        ("signal_mismatch", true, false),
    ] {
        let output = probe(outcome, answered, verified).await?;
        assert_eq!(
            output.status.success(),
            verified,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(result["accepted"], true);
        assert_eq!(result["answered"], answered);
        assert_eq!(result["signal_verified"], verified);
        assert_eq!(result["outcome"], outcome);
    }
    Ok(())
}
