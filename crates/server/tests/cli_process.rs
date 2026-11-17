use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

#[path = "cli_process/music.rs"]
mod music;

fn run(directory: &Path, arguments: &[&OsStr]) -> Result<Output, io::Error> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rusterisk"))
        .current_dir(directory)
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if Instant::now() >= deadline {
            match child.kill() {
                Ok(()) => {}
                Err(error) => {
                    if child.try_wait()?.is_none() {
                        return Err(error);
                    }
                }
            }
            let output = child.wait_with_output()?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "command timed out with {} output bytes",
                    output.stdout.len() + output.stderr.len()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn help_version_and_unknown_options_have_stable_process_results() -> Result<(), io::Error> {
    let directory = tempfile::tempdir()?;
    let help = run(
        directory.path(),
        &[OsStr::new("--help"), OsStr::new("--unknown")],
    )?;
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--config-dir"));
    let version = run(directory.path(), &[OsStr::new("--version")])?;
    assert!(version.status.success());
    assert_eq!(
        version.stdout,
        format!("rusterisk {}\n", env!("CARGO_PKG_VERSION")).as_bytes()
    );
    let unknown = run(directory.path(), &[OsStr::new("--unknown")])?;
    assert_eq!(unknown.status.code(), Some(2));
    assert!(unknown.stdout.is_empty());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("Unknown option"));
    Ok(())
}

#[test]
fn configuration_validation_does_not_bind_ports_or_start_the_server() -> Result<(), io::Error> {
    let directory = tempfile::tempdir()?;
    let settings = directory.path().join("settings with spaces");
    std::fs::create_dir(&settings)?;
    let occupied = std::net::TcpListener::bind("127.0.0.1:0")?;
    let http = format!(
        "[general]\nbindaddr=127.0.0.1\nbindport={}\n",
        occupied.local_addr()?.port()
    );
    for (file, text) in [
        ("http.conf", http.as_str()),
        ("ari.conf", "[app]\ntype=user\npassword=test-password\n"),
        ("pjsip.conf", ""),
        ("rtp.conf", ""),
        ("extensions.conf", ""),
    ] {
        std::fs::write(settings.join(file), text)?;
    }
    let arguments = [
        OsStr::new("--check-config"),
        OsStr::new("--config-dir"),
        settings.as_os_str(),
    ];
    let checked = run(directory.path(), &arguments)?;
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert_eq!(checked.stdout, b"Configuration valid.\n");
    std::fs::write(
        settings.join("http.conf"),
        format!("{http}unexpected=yes\n"),
    )?;
    let invalid = run(directory.path(), &arguments)?;
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("http.conf:4"));
    Ok(())
}

#[test]
fn configuration_validation_reads_optional_media_settings() -> Result<(), io::Error> {
    let directory = tempfile::tempdir()?;
    for (file, text) in [
        ("http.conf", "[general]\nbindaddr=127.0.0.1\n"),
        ("ari.conf", "[app]\ntype=user\npassword=test-password\n"),
        ("pjsip.conf", ""), ("rtp.conf", ""), ("extensions.conf", ""),
        ("chan_websocket.conf", "[global]\ncontrol_message_format=json\n"),
        ("websocket_client.conf", "[agent]\ntype=websocket_client\nconnection_type=per_call_config\nuri=ws://127.0.0.1:8765/media\nprotocols=media\nreconnect_attempts=0\n"),
    ] { std::fs::write(directory.path().join(file), text)?; }
    let arguments = [
        OsStr::new("--check-config"),
        OsStr::new("--config-dir"),
        directory.path().as_os_str(),
    ];
    let invalid = run(directory.path(), &arguments)?;
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("websocket_client.conf:6"));
    std::fs::write(directory.path().join("websocket_client.conf"), "")?;
    assert!(run(directory.path(), &arguments)?.status.success());
    std::fs::write(
        directory.path().join("chan_websocket.conf"),
        "[global]\ncontrol_message_format=invalid\n",
    )?;
    let invalid = run(directory.path(), &arguments)?;
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("chan_websocket.conf:2"));
    Ok(())
}
