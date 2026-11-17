use super::run;
use std::{
    ffi::OsStr,
    fs, io,
    net::{TcpListener, UdpSocket},
    path::Path,
};

#[derive(Clone, Copy)]
enum Settings {
    Http,
    Ari,
    MissingDirectory,
}

impl Settings {
    const fn position(self) -> (&'static str, usize) {
        match self {
            Self::Http => ("http.conf", 4),
            Self::Ari => ("ari.conf", 4),
            Self::MissingDirectory => ("http.conf", 0),
        }
    }
}

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
            "{primary}; startup fixture cleanup failed: {cleanup}"
        ))),
    }
}

fn absent(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::other(format!(
            "Invalid startup created {}",
            path.display()
        ))),
    }
}

fn configuration(directory: &Path, http: &str, ari: &str, sip: &str) -> io::Result<()> {
    fs::create_dir(directory)?;
    for (name, source) in [
        ("http.conf", http),
        ("ari.conf", ari),
        ("pjsip.conf", sip),
        ("rtp.conf", ""),
        ("extensions.conf", ""),
    ] {
        fs::write(directory.join(name), source)?;
    }
    Ok(())
}

fn check(root: &Path, settings: Settings) -> io::Result<()> {
    let selected = root.join("settings with spaces");
    let data = root.join("data");
    let http = TcpListener::bind("127.0.0.1:0")?;
    let sip = UdpSocket::bind("127.0.0.1:0")?;
    let http_address = http.local_addr()?;
    let sip_address = sip.local_addr()?;
    let primary = (|| {
        let http = format!(
            "[general]\nbindaddr=127.0.0.1\nbindport={}\n",
            http_address.port()
        );
        let ari = "[app]\ntype=user\npassword=startup-private-password\n";
        let sip = format!("[udp]\ntype=transport\nprotocol=udp\nbind={sip_address}\n");
        match settings {
            Settings::Http => {
                configuration(&selected, &format!("{http}unexpected=yes\n"), ari, &sip)?
            }
            Settings::Ari => {
                configuration(&selected, &http, &format!("{ari}unexpected=yes\n"), &sip)?
            }
            Settings::MissingDirectory => absent(&selected)?,
        }
        let rejected = run(
            root,
            &[
                OsStr::new("--config-dir"),
                selected.as_os_str(),
                OsStr::new("--data-dir"),
                data.as_os_str(),
            ],
        )?;
        verify(
            rejected.stderr.len() <= 8192,
            "Startup diagnostic exceeded capacity",
        )?;
        let diagnostic = std::str::from_utf8(&rejected.stderr).map_err(io::Error::other)?;
        verify(
            !diagnostic.contains("startup-private-password"),
            "Startup diagnostic disclosed a configured password",
        )?;
        verify(
            rejected.status.code() == Some(2) && rejected.stdout.is_empty(),
            format!(
                "Run settings rejection changed its exit result: {}; {diagnostic}",
                rejected.status
            ),
        )?;
        let (file, line) = settings.position();
        let prefix = format!("rusterisk: {file}:{line}: ");
        verify(
            diagnostic
                .strip_prefix(&prefix)
                .is_some_and(|reason| !reason.trim().is_empty()),
            format!("Run settings rejection lost its position or reason: {diagnostic}"),
        )?;
        match settings {
            Settings::Http | Settings::Ari => verify(
                diagnostic.contains("unsupported option unexpected"),
                "Run settings rejection lost its parser reason",
            ),
            Settings::MissingDirectory => absent(&selected),
        }
    })();
    let mut result = combine(primary, absent(&data));
    result = combine(
        result,
        rusterisk_storage::Store::open(&data)
            .map(drop)
            .map_err(io::Error::other),
    );
    drop(http);
    drop(sip);
    for cleanup in [
        TcpListener::bind(http_address).map(drop),
        UdpSocket::bind(sip_address).map(drop),
    ] {
        result = combine(result, cleanup);
    }
    result
}

fn fixture(settings: Settings) -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let result = check(directory.path(), settings);
    combine(result, directory.close())
}

#[test]
fn run_rejects_malformed_http_and_ari_settings_before_opening_storage() -> io::Result<()> {
    for settings in [Settings::Http, Settings::Ari] {
        fixture(settings)?;
    }
    Ok(())
}

#[test]
fn run_reports_the_missing_configuration_file_without_opening_storage() -> io::Result<()> {
    fixture(Settings::MissingDirectory)
}
