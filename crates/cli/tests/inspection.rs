use axum::{
    http::{HeaderMap, Method, StatusCode, Uri},
    routing::get,
    Router,
};
use serde_json::{json, Value};
use std::{
    io,
    process::{Output, Stdio},
    time::Duration,
};
use tokio::{net::TcpListener, process::Command};

mod support;

fn fixture(
    method: Method,
    uri: &Uri,
    headers: &HeaderMap,
    empty: bool,
) -> (StatusCode, &'static str) {
    let auth = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok());
    if method != Method::GET || auth != Some("Basic cmVhZGVyOnNhbXBsZQ==") {
        return (StatusCode::FORBIDDEN, "");
    }
    let body = match uri.path() {
        "/rusterisk/diagnostics/runtime" => {
            r#"{"control":{"turns":3,"late_turns":1,"drops":0},"signaling":{"turns":5,"late_turns":0,"drops":1},"media":{"turns":21,"late_turns":2,"drops":4}}"#
        }
        "/rusterisk/diagnostics/channels/leg-a" => {
            r#"{"id":"leg-a","generation":8,"creationtime":"t1","endpoint":"desk","state":"Up","held":true}"#
        }
        "/rusterisk/diagnostics/extensions/home/201" => {
            r#"{"context":"home","extension":"201","steps":[{"context":"home","pattern":"201","priority":1,"operation":"Stasis"}],"disposition":"stasis","application":"phone","destinations":[],"channels":["leg-a","leg-b"]}"#
        }
        "/ari/asterisk/info" => {
            r#"{"system":{"version":"22.9.0"},"status":{"startup_time":"2026-11-21T04:25:00Z"}}"#
        }
        "/ari/bridges" if !empty => r#"[{"id":"mix","channels":["leg-a","leg-b"]}]"#,
        "/ari/channels" if !empty => {
            r#"[
            {"id":"leg-a","name":"PJSIP/a","state":"Up","creationtime":"t1","channelvars":{"PRIVATE":"omit-me"}},
            {"id":"leg-b","name":"PJSIP/b","state":"Ringing","creationtime":"t2"}
        ]"#
        }
        "/ari/channels/leg-a" if !empty => {
            r#"{"id":"leg-a","name":"PJSIP/a","state":"Up","creationtime":"t1"}"#
        }
        "/ari/channels" | "/ari/bridges" | "/ari/endpoints" => "[]",
        "/ari/endpoints/PJSIP/static" => {
            r#"{"technology":"PJSIP","resource":"static","state":"online","channel_ids":["leg-a"]}"#
        }
        "/ari/endpoints/PJSIP/expired" => {
            r#"{"technology":"PJSIP","resource":"expired","state":"offline","channel_ids":[]}"#
        }
        _ => return (StatusCode::NOT_FOUND, r#"{"message":"Endpoint not found"}"#),
    };
    (StatusCode::OK, body)
}

fn resources(empty: bool) -> Router {
    Router::new().fallback(get(
        move |method: Method, uri: Uri, headers: HeaderMap| async move {
            fixture(method, &uri, &headers, empty)
        },
    ))
}

async fn inspect(arguments: &[&str], empty: bool) -> io::Result<Output> {
    inspect_with(arguments, resources(empty)).await
}

async fn inspect_with(arguments: &[&str], router: Router) -> io::Result<Output> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let server = format!("http://{}", listener.local_addr()?);
    let mut process = Command::new(env!("CARGO_BIN_EXE_rusterisk-cli"));
    process
        .args(arguments)
        .args([
            "--server",
            &server,
            "--username",
            "reader",
            "--password-stdin",
        ])
        .kill_on_drop(true)
        .stdin(support::password_input(b"sample\n")?)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = process.spawn()?;
    tokio::select! {
        output = tokio::time::timeout(Duration::from_secs(5),child.wait_with_output()) => output?,
        result = axum::serve(listener,router) => Err(io::Error::other(format!("fixture stopped: {result:?}"))),
    }
}

async fn document(arguments: &[&str], empty: bool) -> io::Result<Value> {
    let output = inspect(arguments, empty).await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[tokio::test]
async fn inspection_reports_routing_without_inferring_registration() -> io::Result<()> {
    let status = document(&["status", "--json"], true).await?;
    assert_eq!(status["version"], "22.9.0");
    assert_eq!(status["startup_time"], "2026-11-21T04:25:00Z");
    for command in ["endpoints", "channels"] {
        assert_eq!(document(&[command, "--json"], true).await?, json!([]));
    }
    for (endpoint, state) in [("static", "online"), ("expired", "offline")] {
        let target = format!("PJSIP/{endpoint}");
        let value = document(&["endpoint", &target, "--json"], false).await?;
        assert_eq!(value["routing_state"], state);
        assert_eq!(value["registration"], "unknown");
        assert_eq!(value["resource"], endpoint);
    }
    let missing = inspect(&["endpoint", "PJSIP/missing", "--json"], false).await?;
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("404"));
    Ok(())
}

#[tokio::test]
async fn inspection_counts_legs_and_correlates_bridge_membership() -> io::Result<()> {
    let legs = document(&["channels", "--json"], false).await?;
    for (index, id) in [(0, "leg-a"), (1, "leg-b")] {
        assert_eq!(legs[index]["kind"], "call_leg");
        assert_eq!(legs[index]["id"], id);
        assert_eq!(legs[index]["bridge_ids"], serde_json::to_value(["mix"])?);
    }
    assert!(!legs.to_string().contains("omit-me"));
    assert_eq!(
        document(&["channel", "leg-a", "--json"], false).await?,
        legs[0]
    );
    let text = inspect(&["channels"], false).await?;
    assert!(text.status.success());
    let lines = String::from_utf8_lossy(&text.stdout);
    assert!(lines
        .lines()
        .any(|line| line.contains("Call leg") && line.contains("leg-b") && line.contains("mix")));
    Ok(())
}

#[tokio::test]
async fn doctor_keeps_signaling_when_media_measurements_are_unavailable() -> io::Result<()> {
    let value = document(&["doctor", "channel", "leg-a", "--json"], false).await?;
    assert_eq!(value["state"], "Up");
    assert_eq!(value["held"], true);
    assert_eq!(value["traffic"]["observation"], "unavailable");
    assert!(value["next_check"].as_str().is_some_and(|s| !s.is_empty()));
    let route = document(&["doctor", "extension", "home", "201", "--json"], false).await?;
    assert_eq!(route["application"], "phone");
    assert_eq!(route["channels"].as_array().map(Vec::len), Some(2));
    let terminal = inspect(&["doctor", "extension", "home", "201"], false).await?;
    assert!(terminal.status.success());
    let output = String::from_utf8_lossy(&terminal.stdout);
    assert!(output.contains("phone") && output.contains("leg-b"));
    Ok(())
}

#[tokio::test]
async fn either_packet_read_can_be_unavailable_while_authentication_errors_remain_fatal(
) -> io::Result<()> {
    for (first_succeeds, status) in [
        (false, StatusCode::SERVICE_UNAVAILABLE),
        (true, StatusCode::SERVICE_UNAVAILABLE),
        (false, StatusCode::UNAUTHORIZED),
        (true, StatusCode::FORBIDDEN),
    ] {
        let (first, _reader) = tokio::sync::watch::channel(first_succeeds);
        let app = resources(false).route("/rusterisk/channels/leg-a/rtp-statistics", get(move || {
            let available = first.send_replace(false);
            async move {
                if available {
                    (StatusCode::OK, r#"{"channel_id":"leg-a","creationtime":"t1","generation":8,"observed_at":"2026-11-21T10:00:00.000Z","local":{"source":"server_rtp","rxcount":2,"txcount":3}}"#)
                } else {
                    (status, "{}")
                }
            }
        }));
        let out = inspect_with(&["doctor", "channel", "leg-a", "--json"], app).await?;
        if status == StatusCode::SERVICE_UNAVAILABLE {
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let value: Value = serde_json::from_slice(&out.stdout)?;
            assert_eq!(value["state"], "Up");
            assert_eq!(value["held"], true);
            assert_eq!(value["endpoint"], "desk");
            assert_eq!(value["traffic"]["observation"], "unavailable");
            assert!(value["traffic"]["received"].is_null());
            assert!(value["traffic"]["sent"].is_null());
        } else {
            assert!(!out.status.success());
            assert!(out.stdout.is_empty());
            assert!(String::from_utf8_lossy(&out.stderr).contains(status.as_str()));
        }
    }
    Ok(())
}

#[tokio::test]
async fn runtime_diagnosis_shows_each_processing_loop() -> io::Result<()> {
    let report = document(&["doctor", "runtime", "--json"], true).await?;
    assert_eq!(report["media"]["late_turns"], 2);
    let output = inspect(&["doctor", "runtime"], true).await?;
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).map_err(io::Error::other)?;
    assert!(text.contains("media") && text.contains("21") && text.contains("drops"));
    Ok(())
}
