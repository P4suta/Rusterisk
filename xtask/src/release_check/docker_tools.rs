use crate::{process::CommandSupervisor, release_check::native_artifacts::read_regular};
use serde::Deserialize;
use std::{
    fs, io,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Version {
    client: Tool,
    server: Tool,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Tool {
    version: String,
    git_commit: String,
    os: String,
    arch: String,
}

fn invalid() -> io::Error {
    io::Error::other("retained Docker client or server identity differs from its native profile")
}

fn tool(tool: &Tool) -> io::Result<()> {
    semver::Version::parse(&tool.version).map_err(io::Error::other)?;
    if !(7..=64).contains(&tool.git_commit.len())
        || !tool.git_commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !matches!(tool.arch.as_str(), "amd64" | "arm64")
    {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn check(root: &Path, os: &str, architecture: &str) -> io::Result<()> {
    let output = read_regular(&root.join("tools/docker-version.stdout.json"), 65_536)?;
    if !read_regular(&root.join("tools/docker-version.stderr.txt"), 4096)?.is_empty() {
        return Err(invalid());
    }
    let version: Version = serde_json::from_slice(&output).map_err(io::Error::other)?;
    tool(&version.client)?;
    tool(&version.server)?;
    let (client_os, client_arch) = match (os, architecture) {
        ("macos", "aarch64") => ("darwin", "arm64"),
        ("macos", "x86_64") => ("darwin", "amd64"),
        ("linux", "aarch64") => ("linux", "arm64"),
        ("linux", "x86_64") => ("linux", "amd64"),
        _ => return Err(invalid()),
    };
    if version.client.os != client_os
        || version.client.arch != client_arch
        || version.server.os != "linux"
    {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn capture(root: &Path, supervisor: &mut CommandSupervisor) -> io::Result<()> {
    let directory = root.join("tools");
    let stdout = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("docker-version.stdout.json"))?;
    let stderr = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("docker-version.stderr.txt"))?;
    let mut command = Command::new("docker");
    command
        .args(["version", "--format", "{{json .}}"])
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    supervisor
        .run(&mut command, Duration::from_secs(15))
        .map_err(io::Error::other)?;
    check(root, std::env::consts::OS, std::env::consts::ARCH)
}
