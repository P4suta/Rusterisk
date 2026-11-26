#![cfg(unix)]

use super::run;
use rusterisk_test_support::Wait;
use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{self, Read},
    net::{TcpListener, UdpSocket},
    os::unix::fs::FileTypeExt,
    path::Path,
    process::{Child, Command, Stdio},
};

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
            "{primary}; TLS file fixture cleanup failed: {cleanup}"
        ))),
    }
}

fn absent(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::other(format!(
            "TLS validation created {}",
            path.display()
        ))),
    }
}

fn read(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(8193).read_to_end(&mut bytes)?;
    verify(bytes.len() <= 8192, "mkfifo diagnostics exceeded capacity")?;
    Ok(bytes)
}

fn kill(child: &mut Child) -> io::Result<()> {
    match child.kill() {
        Ok(()) => Ok(()),
        Err(error) => match child.try_wait() {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(error),
            Err(cleanup) => combine(Err(error), Err(cleanup)),
        },
    }
}

fn fifo(path: &Path) -> io::Result<()> {
    let stdout = path.with_extension("mkfifo-stdout");
    let stderr = path.with_extension("mkfifo-stderr");
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stdout)?;
    let diagnostic = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stderr)?;
    let mut child = Command::new("mkfifo")
        .args(["-m", "600"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(output)
        .stderr(diagnostic)
        .spawn()?;
    let operation = rusterisk_test_support::child_exit(&mut child, Wait::Ready);
    let stopped = if operation.is_err() {
        kill(&mut child)
    } else {
        Ok(())
    };
    let reaped = child.wait().map(|_| ());
    let operation = combine(operation, combine(stopped, reaped));
    let observed = (|| {
        let stdout = read(&stdout)?;
        let stderr = read(&stderr)?;
        Ok((stdout, stderr))
    })();
    let (status, stdout, stderr) = match (operation, observed) {
        (Ok(status), Ok((stdout, stderr))) => (status, stdout, stderr),
        (Err(error), Ok((stdout, stderr))) => {
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "{error}; mkfifo stdout={}; stderr={}",
                    String::from_utf8_lossy(&stdout),
                    String::from_utf8_lossy(&stderr)
                ),
            ))
        }
        (operation, Err(cleanup)) => return combine(operation.map(|_| ()), Err(cleanup)),
    };
    verify(
        status.success() && stdout.is_empty() && stderr.is_empty(),
        format!(
            "mkfifo fixture setup failed: {status}; stdout={}; stderr={}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        ),
    )?;
    verify(
        fs::symlink_metadata(path)?.file_type().is_fifo(),
        "mkfifo fixture did not create a FIFO",
    )
}

#[derive(Clone, Copy)]
enum Material {
    Certificate,
    PrivateKey,
}

impl Material {
    const fn file(self) -> &'static str {
        match self {
            Self::Certificate => "certificate.pem",
            Self::PrivateKey => "private-key.pem",
        }
    }
}

fn storage_untouched(root: &Path, settings: &Path) -> io::Result<()> {
    let mut result = absent(&root.join("missing-storage"));
    result = combine(result, absent(&root.join("data")));
    for directory in [root, settings] {
        for artifact in [
            ".rusterisk.lock",
            "calls.sqlite3",
            "calls.sqlite3-wal",
            "calls.sqlite3-shm",
            "recordings",
        ] {
            result = combine(result, absent(&directory.join(artifact)));
        }
    }
    result
}

fn check(root: &Path, material: Material) -> io::Result<()> {
    let settings = root.join("settings with spaces");
    let data = root.join("missing-storage/selected");
    let tls = TcpListener::bind("127.0.0.1:0")?;
    let sip = UdpSocket::bind("127.0.0.1:0")?;
    let tls_address = tls.local_addr()?;
    let sip_address = sip.local_addr()?;
    let operation = (|| {
        fs::create_dir(&settings)?;
        let certificate = settings.join("certificate.pem");
        let key = settings.join("private-key.pem");
        fs::write(&certificate, b"not a certificate\n")?;
        fs::write(&key, b"not a private key\n")?;
        let http = format!(
            "[general]\nbindaddr=127.0.0.1\nbindport={}\ntlsenable=yes\ntlsbindaddr={tls_address}\ntlscertfile={}\ntlsprivatekey={}\n",
            tls_address.port(), certificate.display(), key.display()
        );
        let pjsip = format!("[udp]\ntype=transport\nprotocol=udp\nbind={sip_address}\n");
        for (name, source) in [
            ("http.conf", http.as_str()),
            ("ari.conf", "[app]\ntype=user\npassword=test-password\n"),
            ("pjsip.conf", pjsip.as_str()),
            ("rtp.conf", ""),
            ("extensions.conf", ""),
        ] {
            fs::write(settings.join(name), source)?;
        }
        let arguments = [
            OsStr::new("--check-config"),
            OsStr::new("--config-dir"),
            settings.as_os_str(),
            OsStr::new("--data-dir"),
            data.as_os_str(),
        ];
        let regular = run(root, &arguments).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("regular TLS fixture control: {error}"),
            )
        })?;
        let diagnostic = String::from_utf8_lossy(&regular.stderr[..regular.stderr.len().min(8192)]);
        verify(
            regular.status.code() == Some(1)
                && regular.stdout.is_empty()
                && regular.stderr.len() <= 8192
                && diagnostic.starts_with("rusterisk:")
                && !diagnostic.to_ascii_lowercase().contains("regular file")
                && !diagnostic.contains("panicked at"),
            format!("regular invalid-PEM control did not reach TLS validation: {diagnostic}"),
        )?;
        storage_untouched(root, &settings)?;
        let selected = settings.join(material.file());
        fs::remove_file(&selected)?;
        fifo(&selected).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("{} FIFO fixture setup: {error}", material.file()),
            )
        })?;
        let other = match material {
            Material::Certificate => &key,
            Material::PrivateKey => &certificate,
        };
        let metadata = fs::symlink_metadata(other)?;
        verify(
            metadata.is_file() && metadata.len() > 0,
            "Other TLS input is not a nonempty regular file",
        )?;
        let rejected = run(root, &arguments).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("{} FIFO validation: {error}", material.file()),
            )
        })?;
        let diagnostic =
            String::from_utf8_lossy(&rejected.stderr[..rejected.stderr.len().min(8192)]);
        verify(
            rejected.status.code() == Some(1)
                && rejected.stdout.is_empty()
                && rejected.stderr.len() <= 8192
                && diagnostic.starts_with("rusterisk:")
                && diagnostic.to_ascii_lowercase().contains("regular file")
                && !diagnostic.contains("panicked at"),
            format!(
                "{} FIFO lacked an explicit regular-file rejection: {diagnostic}",
                material.file()
            ),
        )
    })();
    combine(operation, storage_untouched(root, &settings))
}

fn fixture(material: Material) -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let result = check(directory.path(), material);
    combine(result, directory.close())
}

#[test]
fn certificate_fifo_is_rejected_without_opening_storage_or_binding_listeners() -> io::Result<()> {
    fixture(Material::Certificate)
}

#[test]
fn private_key_fifo_is_rejected_after_reading_the_regular_certificate_input() -> io::Result<()> {
    fixture(Material::PrivateKey)
}
