use super::run;
use rusterisk_media::Frame;
use std::ffi::OsStr;
use std::io;
use std::path::Path;

#[test]
fn music_check_uses_the_explicit_data_root_and_rejects_assets_at_the_source_position(
) -> Result<(), io::Error> {
    fixture(|root| {
        let settings = root.join("settings with spaces");
        let data = root.join("audio with spaces");
        let default = root.join("data");
        configuration(
            &settings,
            "[general]\nbindaddr=127.0.0.1\n",
            "",
            "[default]\nmode=files\ndirectory=music\n",
        )?;
        for directory in [&data, &default] {
            std::fs::create_dir_all(directory.join("music"))?;
            std::fs::write(directory.join("music/bad.wav"), b"not audio")?;
        }
        let arguments = [
            OsStr::new("--check-config"),
            OsStr::new("--config-dir"),
            settings.as_os_str(),
            OsStr::new("--data-dir"),
            data.as_os_str(),
        ];
        let invalid = run(root, &arguments)?;
        let error = String::from_utf8_lossy(&invalid.stderr);
        verify(
            !invalid.status.success()
                && invalid.stdout.is_empty()
                && error.contains("musiconhold.conf:3")
                && !error.contains("panicked at"),
            format!("music check accepted invalid assets or lost the source position: {error}"),
        )?;
        std::fs::remove_file(data.join("music/bad.wav"))?;
        let staging = root.join("staging");
        let mut recorder = rusterisk_storage::Recorder::create(&staging, "known", 8000)
            .map_err(io::Error::other)?;
        recorder
            .write(Frame::new(&[1234; 160], 8000).map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
        std::fs::rename(
            recorder.finish().map_err(io::Error::other)?,
            data.join("music/known.wav"),
        )?;
        let valid = run(root, &arguments)?;
        verify(
            valid.status.success()
                && valid.stdout == b"Configuration valid.\n"
                && valid.stderr.is_empty(),
            format!(
                "music check ignored the explicit data directory: {}",
                String::from_utf8_lossy(&valid.stderr)
            ),
        )?;
        for directory in [&data, &default] {
            for artifact in [
                ".rusterisk.lock",
                "calls.sqlite3",
                "calls.sqlite3-wal",
                "calls.sqlite3-shm",
                "recordings",
            ] {
                verify(
                    !directory.join(artifact).exists(),
                    format!("music check created storage artifact {artifact}"),
                )?;
            }
        }
        Ok(())
    })
}

#[test]
fn empty_music_check_neither_creates_a_missing_data_root_nor_binds_configured_ports(
) -> Result<(), io::Error> {
    fixture(|root| {
        let settings = root.join("settings");
        let missing = root.join("missing/audio");
        let http = std::net::TcpListener::bind("127.0.0.1:0")?;
        let sip = std::net::UdpSocket::bind("127.0.0.1:0")?;
        let http_address = http.local_addr()?;
        let sip_address = sip.local_addr()?;
        let result = (|| {
            configuration(
                &settings,
                &format!(
                    "[general]\nbindaddr=127.0.0.1\nbindport={}\n",
                    http_address.port()
                ),
                &format!("[udp]\ntype=transport\nprotocol=udp\nbind={sip_address}\n"),
                "",
            )?;
            let checked = run(
                root,
                &[
                    OsStr::new("--check-config"),
                    OsStr::new("--config-dir"),
                    settings.as_os_str(),
                    OsStr::new("--data-dir"),
                    missing.as_os_str(),
                ],
            )?;
            verify(
                checked.status.success()
                    && checked.stdout == b"Configuration valid.\n"
                    && checked.stderr.is_empty(),
                format!(
                    "empty music check attempted startup: {}",
                    String::from_utf8_lossy(&checked.stderr)
                ),
            )?;
            verify(
                !root.join("missing").exists() && !root.join("data").exists(),
                "empty music check created a data root",
            )
        })();
        drop(http);
        drop(sip);
        let http = std::net::TcpListener::bind(http_address).map(drop);
        let sip = std::net::UdpSocket::bind(sip_address).map(drop);
        combine(result, combine(http, sip))
    })
}

fn configuration(root: &Path, http: &str, sip: &str, music: &str) -> Result<(), io::Error> {
    std::fs::create_dir(root)?;
    for (name, source) in [
        ("http.conf", http),
        ("ari.conf", "[app]\ntype=user\npassword=test-password\n"),
        ("pjsip.conf", sip),
        ("rtp.conf", ""),
        ("extensions.conf", ""),
        ("musiconhold.conf", music),
    ] {
        std::fs::write(root.join(name), source)?;
    }
    Ok(())
}

fn verify(condition: bool, message: impl std::fmt::Display) -> Result<(), io::Error> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message.to_string()))
    }
}

fn combine(
    primary: Result<(), io::Error>,
    cleanup: Result<(), io::Error>,
) -> Result<(), io::Error> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
            "{primary}; music check cleanup failed: {cleanup}"
        ))),
    }
}

fn fixture(check: impl FnOnce(&Path) -> Result<(), io::Error>) -> Result<(), io::Error> {
    let directory = tempfile::tempdir()?;
    let result = check(directory.path());
    combine(result, directory.close())
}
