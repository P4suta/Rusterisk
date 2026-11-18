use crate::recorded_path::{RecordedPath, RecordedPathFormat::Windows};
use serde::Deserialize;
use std::{collections::BTreeSet, io, path::Path};
#[cfg(windows)]
use {crate::process::SupervisedCommand, std::path::PathBuf};

const OUTPUT_LIMIT: usize = 65_536;
const MODULE_LIMIT: usize = 64;

#[cfg(windows)]
const QUERY: &str = r"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$server = [Diagnostics.Process]::GetProcessById([int]$env:RUSTERISK_DEPENDENCY_PID)
try {
    $server.Refresh()
    $modules = @($server.Modules | Select-Object -First 65 -ExpandProperty FileName)
    [ordered]@{
        format = 1
        observer_bits = [IntPtr]::Size * 8
        observer_version = $PSVersionTable.PSVersion.ToString()
        executable = $server.MainModule.FileName
        system_directory = [Environment]::SystemDirectory
        modules = $modules
    } | ConvertTo-Json -Depth 4 -Compress
} finally {
    $server.Dispose()
}
";

#[cfg(windows)]
pub(super) fn command(process_id: u32) -> io::Result<SupervisedCommand> {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .ok_or_else(invalid)?;
    Windows.parse(&root, 4096)?;
    let program = root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let mut command = SupervisedCommand::new(program).map_err(io::Error::other)?;
    for argument in [
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        QUERY,
    ] {
        command.arg(argument).map_err(io::Error::other)?;
    }
    command
        .env("RUSTERISK_DEPENDENCY_PID", process_id.to_string())
        .map_err(io::Error::other)?;
    Ok(command)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    format: u8,
    observer_bits: u8,
    observer_version: String,
    executable: String,
    system_directory: String,
    modules: Vec<String>,
}

fn invalid() -> io::Error {
    io::Error::other("Windows loaded module observation differs from the native profile")
}

fn observer_version(version: &str) -> bool {
    let mut parts = version.split('.');
    let number =
        |part: &str| part.bytes().all(|byte| byte.is_ascii_digit()) && part.parse::<u32>().is_ok();
    parts.next() == Some("5")
        && parts.next() == Some("1")
        && parts.next().is_some_and(number)
        && parts.next().is_some_and(number)
        && parts.next().is_none()
}

#[cfg(any(windows, test))]
pub(super) fn check_output(stdout: &[u8], stderr: &[u8], executable: &str) -> io::Result<()> {
    check_output_for_path(stdout, stderr, &Windows.parse(Path::new(executable), 4096)?)
}

pub(super) fn check_output_for_path(
    stdout: &[u8],
    stderr: &[u8],
    executable: &RecordedPath,
) -> io::Result<()> {
    if !stderr.is_empty() || stdout.len() > OUTPUT_LIMIT {
        return Err(invalid());
    }
    let observed: Observation = serde_json::from_slice(stdout).map_err(io::Error::other)?;
    if observed.format != 1
        || observed.observer_bits != 64
        || !observer_version(&observed.observer_version)
        || !(2..=MODULE_LIMIT).contains(&observed.modules.len())
    {
        return Err(invalid());
    }
    if Windows.parse(Path::new(&observed.executable), 4096)? != *executable
        || !executable
            .file_name()
            .is_some_and(|name| name.ends_with(".exe"))
    {
        return Err(invalid());
    }
    let system = Windows.parse(Path::new(&observed.system_directory), 4096)?;
    if system.file_name() != Some("system32") {
        return Err(invalid());
    }
    let mut seen = BTreeSet::new();
    for module in &observed.modules {
        let module = Windows.parse(Path::new(module), 4096)?;
        if module != *executable
            && (!module.starts_with(&system)
                || !module
                    .file_name()
                    .is_some_and(|name| name.ends_with(".dll")))
        {
            return Err(invalid());
        }
        if !seen.insert(module) {
            return Err(invalid());
        }
    }
    if !seen.contains(executable) {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
