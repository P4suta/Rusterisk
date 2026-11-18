use crate::{
    process::CommandSupervisor,
    release_check::{native_artifacts::read_regular, CapturedReleaseSource},
};
use std::{
    fs, io,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

fn invalid() -> io::Error {
    io::Error::other("native tool outputs differ from the captured toolchain and host profile")
}

fn field<'a>(text: &'a str, key: &str) -> io::Result<&'a str> {
    let mut values = text.lines().filter_map(|line| line.strip_prefix(key));
    let value = values.next().ok_or_else(invalid)?;
    if value.is_empty() || value.len() > 256 || values.next().is_some() {
        return Err(invalid());
    }
    Ok(value)
}

fn channel(captured: &CapturedReleaseSource) -> io::Result<String> {
    let bytes = captured
        .files()
        .find(|(path, _)| *path == Path::new("rust-toolchain.toml"))
        .map(|(_, bytes)| bytes)
        .ok_or_else(invalid)?;
    let value: toml::Value = toml::from_str(std::str::from_utf8(bytes).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;
    let channel = value
        .get("toolchain")
        .and_then(|table| table.get("channel"))
        .and_then(toml::Value::as_str)
        .ok_or_else(invalid)?;
    semver::Version::parse(channel).map_err(io::Error::other)?;
    Ok(channel.to_owned())
}

pub(in crate::release_check) fn configure(
    captured: &CapturedReleaseSource,
    command: &mut Command,
) -> io::Result<()> {
    command.env("RUSTUP_TOOLCHAIN", channel(captured)?);
    Ok(())
}

pub(in crate::release_check) fn check(
    captured: &CapturedReleaseSource,
    directory: &Path,
    os: &str,
    arch: &str,
) -> io::Result<()> {
    let expected = channel(captured)?;
    let host = match (os, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        _ => return Err(invalid()),
    };
    for program in ["rustc", "cargo"] {
        let output = read_regular(&directory.join(format!("{program}.stdout.txt")), 65_536)?;
        if !read_regular(&directory.join(format!("{program}.stderr.txt")), 4096)?.is_empty() {
            return Err(invalid());
        }
        let text = std::str::from_utf8(&output).map_err(io::Error::other)?;
        let first = text.lines().next().ok_or_else(invalid)?;
        if !first.starts_with(&format!("{program} {expected} ("))
            || field(text, "release: ")? != expected
            || field(text, "host: ")? != host
        {
            return Err(invalid());
        }
        let hash = field(text, "commit-hash: ")?;
        if hash.len() != 40 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid());
        }
    }
    Ok(())
}

pub(in crate::release_check) fn capture(
    captured: &CapturedReleaseSource,
    root: &Path,
    supervisor: &mut CommandSupervisor,
) -> io::Result<()> {
    let directory = root.join("tools");
    fs::create_dir(&directory)?;
    for program in ["rustc", "cargo"] {
        let stdout = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(format!("{program}.stdout.txt")))?;
        let stderr = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(format!("{program}.stderr.txt")))?;
        let mut command = Command::new(program);
        configure(captured, &mut command)?;
        command
            .arg("--version")
            .arg("--verbose")
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
        supervisor
            .run(&mut command, Duration::from_secs(15))
            .map_err(io::Error::other)?;
    }
    check(
        captured,
        &directory,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}
