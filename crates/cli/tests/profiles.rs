use axum::{
    http::{HeaderMap, StatusCode},
    routing::get,
    Router,
};
use serde_json::json;
use std::{
    io,
    path::Path,
    process::{Output, Stdio},
    time::Duration,
};
use tokio::{net::TcpListener, process::Command};

mod support;

#[tokio::test]
async fn ambiguous_profiles_and_extra_credentials_report_no_private_input() -> io::Result<()> {
    let config = tempfile::tempdir()?;
    let folder = config.path().join("rusterisk");
    std::fs::create_dir(&folder)?;
    let profile = r#"{"name":"lab","server":"https://pbx.example.org","username":"reader"}"#;
    for contents in [
        format!(r#"{{"profiles":[{profile},{profile}]}}"#),
        r#"{"profiles":[{"name":"lab","server":"https://pbx.example.org","username":"reader","password":"private-field"}]}"#.to_owned(),
        r#"{"profiles":[{"name":"lab","server":"https://pbx.example.org/folder/..","username":"reader"}]}"#.to_owned(),
    ] {
        std::fs::write(folder.join("profiles.json"),&contents)?;
        let output = invoke(config.path(),&["profiles"],b"").await?;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-field"));
    }
    Ok(())
}

async fn invoke(config: &Path, arguments: &[&str], password: &[u8]) -> io::Result<Output> {
    let child = Command::new(env!("CARGO_BIN_EXE_rusterisk-cli"))
        .env("XDG_CONFIG_HOME", config)
        .args(arguments)
        .kill_on_drop(true)
        .stdin(support::password_input(password)?)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await?
}

fn server(auth: &'static str, name: &'static str) -> Router {
    Router::new().route(
        "/ari/asterisk/info",
        get(move |headers: HeaderMap| async move {
            if headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                != Some(auth)
            {
                return (StatusCode::UNAUTHORIZED, String::new());
            }
            (
                StatusCode::OK,
                json!({"system":{"version":name},"status":{"startup_time":"now"}}).to_string(),
            )
        }),
    )
}

#[tokio::test]
async fn profiles_and_explicit_options_select_their_server_and_identity() -> io::Result<()> {
    let config = tempfile::tempdir()?;
    std::fs::create_dir(config.path().join("rusterisk"))?;
    let path = config.path().join("rusterisk/profiles.json");
    let a = TcpListener::bind("127.0.0.1:0").await?;
    let b = TcpListener::bind("127.0.0.1:0").await?;
    let a_url = format!("http://{}", a.local_addr()?);
    let b_url = format!("http://{}", b.local_addr()?);
    let bytes = format!(
        r#"{{"profiles":[{{"name":"lab","server":"{a_url}","username":"alpha"}},{{"name":"live","server":"{b_url}","username":"beta"}}]}}"#
    );
    std::fs::write(&path, &bytes)?;
    let cases = async {
        for (name, expected) in [("lab", "alpha-server"), ("live", "beta-server")] {
            let out = invoke(
                config.path(),
                &["status", "--profile", name, "--password-stdin", "--json"],
                b"fixture-secret\n",
            )
            .await?;
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&out.stdout)?["version"],
                expected
            );
            assert!(!String::from_utf8_lossy(&out.stdout).contains("fixture-secret"));
            assert!(out.stderr.is_empty());
        }
        let out = invoke(
            config.path(),
            &[
                "status",
                "--server",
                &b_url,
                "--username",
                "beta",
                "--password-stdin",
                "--json",
            ],
            b"fixture-secret\n",
        )
        .await?;
        assert!(out.status.success());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&out.stdout)?["version"],
            "beta-server"
        );
        assert_eq!(std::fs::read_to_string(&path)?, bytes);
        Ok::<_, io::Error>(())
    };
    tokio::select! {
        result = cases => result,
        result = axum::serve(a,server("Basic YWxwaGE6Zml4dHVyZS1zZWNyZXQ=","alpha-server")) => Err(io::Error::other(format!("fixture stopped: {result:?}"))),
        result = axum::serve(b,server("Basic YmV0YTpmaXh0dXJlLXNlY3JldA==","beta-server")) => Err(io::Error::other(format!("fixture stopped: {result:?}"))),
    }
}

#[tokio::test]
async fn offline_listing_and_invalid_selection_never_contact_a_server() -> io::Result<()> {
    let config = tempfile::tempdir()?;
    std::fs::create_dir(config.path().join("rusterisk"))?;
    let path = config.path().join("rusterisk/profiles.json");
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let document =
        format!(r#"{{"profiles":[{{"name":"lab","server":"{origin}","username":"reader"}}]}}"#);
    std::fs::write(&path, &document)?;
    let out = invoke(config.path(), &["profiles"], b"").await?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("lab"));
    for args in [
        &["ping", "--profile", "unknown", "--password-stdin"][..],
        &[
            "ping",
            "--profile",
            "lab",
            "--server",
            &origin,
            "--password-stdin",
        ],
        &[
            "ping",
            "--username",
            "reader",
            "--profile",
            "lab",
            "--password-stdin",
        ],
    ] {
        let failed = invoke(config.path(), args, b"fixture-secret\n").await?;
        assert_eq!(failed.status.code(), Some(2));
        assert!(failed.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&failed.stderr).contains("fixture-secret"));
    }
    let invalid = document.replace(&origin, &format!("{origin}/ari"));
    std::fs::write(&path, &invalid)?;
    let failed = invoke(
        config.path(),
        &["ping", "--profile", "lab", "--password-stdin"],
        b"fixture-secret\n",
    )
    .await?;
    assert_eq!(failed.status.code(), Some(2));
    assert!(
        tokio::time::timeout(Duration::from_millis(35), listener.accept())
            .await
            .is_err()
    );
    Ok(())
}
