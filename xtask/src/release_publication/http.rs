use super::{candidate, invalid, registry, Release};
use serde_json::Value;
use std::{
    fs,
    io::{self, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

struct Response {
    status: u16,
    body: Value,
}

fn request(url: &str, upload: Option<(&Path, &str)>) -> io::Result<Response> {
    let response = tempfile::NamedTempFile::new()?;
    let mut curl = Command::new("curl");
    curl.env_remove("CRATES_IO_PUBLISH_TOKEN")
        .env_remove("RELEASE_SETTINGS_TOKEN")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .args([
            "--disable",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--connect-timeout",
            "10",
            "--max-time",
            "45",
            "--user-agent",
            "Rusterisk release workflow (https://github.com/P4suta/Rusterisk)",
            "--header",
            "Accept: application/json",
            "--header",
            "Cache-Control: no-cache",
            "--output",
        ])
        .arg(response.path())
        .args(["--write-out", "%{http_code}"]);
    if let Some((path, _)) = upload {
        curl.args([
            "--request",
            "PUT",
            "--header",
            "@-",
            "--header",
            "Content-Type: application/octet-stream",
            "--data-binary",
        ])
        .arg(format!("@{}", path.display()))
        .stdin(Stdio::piped());
    }
    let mut child = curl.arg(url).stdout(Stdio::piped()).spawn()?;
    let write = if let Some((_, token)) = upload {
        child
            .stdin
            .take()
            .ok_or_else(|| invalid("missing registry header input"))
            .and_then(|mut input| writeln!(input, "Authorization: {token}"))
    } else {
        Ok(())
    };
    let result = child.wait_with_output()?;
    write?;
    if !result.status.success() {
        return Err(io::Error::other(format!(
            "crates.io transport failed with {}",
            result.status
        )));
    }
    let status = std::str::from_utf8(&result.stdout)
        .map_err(io::Error::other)?
        .trim()
        .parse::<u16>()
        .map_err(io::Error::other)?;
    let body = serde_json::from_slice(&candidate::small_file(response.path(), 1024 * 1024)?)?;
    Ok(Response { status, body })
}

pub(super) fn present(release: &Release<'_>, checksum: &str) -> io::Result<bool> {
    let response = request(
        &format!(
            "https://crates.io/api/v1/crates/rusterisk-cli/{}",
            release.version
        ),
        None,
    )?;
    match response.status {
        200 => {
            registry::verify_receipt(release, checksum, &response.body)?;
            Ok(true)
        }
        404 if response.body["errors"].is_array() => Ok(false),
        status => Err(io::Error::other(format!(
            "crates.io version lookup returned HTTP {status}"
        ))),
    }
}

pub(super) fn publish(release: &Release<'_>, checksum: &str, archive: &[u8]) -> io::Result<()> {
    if candidate::sha256(archive) != checksum {
        return Err(invalid("registry archive checksum mismatch"));
    }
    let readme = candidate::check_crate(release, archive)?;
    if present(release, checksum)? {
        return Ok(());
    }
    let token = std::env::var("CRATES_IO_PUBLISH_TOKEN")
        .map_err(|_| invalid("missing Trusted Publishing token"))?;
    if token.is_empty() || !token.bytes().all(|c| c.is_ascii_graphic()) {
        return Err(invalid("invalid Trusted Publishing token"));
    }
    let wire = tempfile::NamedTempFile::new()?;
    fs::write(
        wire.path(),
        registry::upload_body(release, &readme, archive)?,
    )?;
    let upload = request(
        "https://crates.io/api/v1/crates/new",
        Some((wire.path(), &token)),
    )
    .and_then(|r| {
        if (200..300).contains(&r.status) && r.body.get("errors").is_none() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "crates.io upload returned HTTP {}: {}",
                r.status, r.body["errors"]
            )))
        }
    });
    for attempt in 0..8 {
        if present(release, checksum)? {
            return Ok(());
        }
        if attempt < 7 {
            thread::sleep(Duration::from_secs(3));
        }
    }
    upload?;
    Err(io::Error::other(
        "registry receipt is pending; reconcile the version before retrying",
    ))
}
