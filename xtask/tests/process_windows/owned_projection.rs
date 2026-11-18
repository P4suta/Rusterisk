#![cfg(windows)]

use std::{
    ffi::{OsStr, OsString},
    fs, io,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use xtask::process::{self, CommandSupervisor, ProcessError, SupervisedCommand};

#[path = "../owned_command/windows_output.rs"]
mod output;
use output::Output;

const MODE: &str = "RUSTERISK_WINDOWS_PROJECTION_WORKER";
const DIRECTORY: &str = "RUSTERISK_WINDOWS_PROJECTION_DIRECTORY";
const VALUE: &str = "RUSTERISK_WINDOWS_PROJECTION_VALUE";
const CASE_FIRST: &str = "RUSTERISK_PROJECTION_\u{017f}";
const CASE_SECOND: &str = "RUSTERISK_PROJECTION_S";
const CASE_ALIAS: &str = "RUSTERISK_PROJECTION_s";
const ASCII_FIRST: &str = "RUSTERISK_PROJECTION_Mixed";
const ASCII_ALIAS: &str = "RUSTERISK_PROJECTION_MIXED";
const PARENT: &str = "RUSTERISK_WINDOWS_PROJECTION_PARENT";
const WORKER: &str = "owned_projection::projection_worker";
const PARENT_WORKER: &str = "owned_projection::inherited_environment_parent";
const ARGUMENT: &str = "projection \u{03bc} \u{3042} \\\"quoted\\\"";
const WIDE_VALUE: [u16; 8] = [
    0x0062, 0x006f, 0x0075, 0x006e, 0x0064, 0xd800, 0x0065, 0x0064,
];

#[derive(Clone, Copy)]
enum Adapter {
    Standard,
    Owned,
}

#[derive(Clone, Copy)]
enum Environment {
    Explicit,
    Inherited,
    SetInherited,
    RemoveInherited,
}

fn checked<T>(result: Result<T, impl std::fmt::Display>) -> io::Result<T> {
    result.map_err(|error| io::Error::other(error.to_string()))
}

fn verify(valid: bool, reason: &'static str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(reason))
    }
}

fn combine(
    operation: Result<(), ProcessError>,
    cleanup: Result<(), ProcessError>,
) -> Result<(), ProcessError> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(ProcessError::Cleanup {
            operation: operation.to_string(),
            cleanup: cleanup.to_string(),
        }),
    }
}

fn projection(stdout: &str) -> io::Result<String> {
    let mut lines = stdout.lines().filter_map(|line| {
        line.split_once("projection aliases=")
            .map(|(_, value)| value)
    });
    let value = lines
        .next()
        .ok_or_else(|| io::Error::other("Windows projection worker output is missing"))?;
    verify(
        lines.next().is_none(),
        "Windows projection worker output is duplicated",
    )?;
    Ok(value.to_owned())
}

fn request(directory: &Path, environment: Environment) -> io::Result<SupervisedCommand> {
    let mut request = checked(SupervisedCommand::new(std::env::current_exe()?))?;
    for argument in [
        "--exact",
        WORKER,
        "--nocapture",
        "--test-threads=1",
        "--skip",
        ARGUMENT,
    ] {
        checked(request.arg(argument))?;
    }
    match environment {
        Environment::Explicit => {
            checked(request.env_clear())?;
            checked(request.env(MODE, "1"))?;
            checked(request.env(DIRECTORY, directory))?;
            checked(request.env(VALUE, OsString::from_wide(&WIDE_VALUE)))?;
            checked(request.env(CASE_FIRST, "first"))?;
            checked(request.env(CASE_SECOND, "latest"))?;
        }
        Environment::Inherited => {}
        Environment::SetInherited => {
            checked(request.env(ASCII_ALIAS, "updated-ascii"))?;
            checked(request.env(CASE_ALIAS, "updated-s"))?;
        }
        Environment::RemoveInherited => {
            checked(request.env_remove(ASCII_ALIAS))?;
            checked(request.env_remove(CASE_ALIAS))?;
        }
    }
    checked(request.current_dir(directory))?;
    Ok(request)
}

fn run(
    adapter: Adapter,
    directory: &Path,
    environment: Environment,
) -> Result<String, ProcessError> {
    let mut request = request(directory, environment)?;
    let output = Output::capture(&mut request)?;
    let budget = Duration::from_secs(5);
    let operation = match adapter {
        Adapter::Standard => {
            let mut command = request.into_std();
            let result = process::run(&mut command, budget);
            drop(command);
            result
        }
        Adapter::Owned => (|| {
            let mut supervisor = CommandSupervisor::new()?;
            let operation = supervisor.run_owned(request, budget);
            combine(operation, supervisor.finish())
        })(),
    };
    match (operation, output.read()) {
        (Ok(()), Ok((stdout, stderr))) => {
            verify(
                stderr == "projection stderr\n",
                "Windows projection stderr differs",
            )?;
            projection(&stdout).map_err(ProcessError::Io)
        }
        (Err(error), Ok((stdout, stderr))) => {
            eprintln!("{stdout}{stderr}");
            Err(error)
        }
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(operation), Err(cleanup)) => Err(ProcessError::Cleanup {
            operation: operation.to_string(),
            cleanup: cleanup.to_string(),
        }),
    }
}

fn compare(directory: &Path, environment: Environment) -> Result<(), ProcessError> {
    let expected = run(Adapter::Standard, directory, environment)?;
    let actual = run(Adapter::Owned, directory, environment)?;
    verify(
        actual == expected,
        "Windows owned projection changed environment name semantics",
    )?;
    Ok(())
}

fn within_directory(
    test: impl FnOnce(&Path) -> Result<(), ProcessError>,
) -> Result<(), ProcessError> {
    let parent = fs::canonicalize(std::env::temp_dir())?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for attempt in 0..32 {
        let root = parent.join(format!(
            "rusterisk-windows-projection-{}-{stamp}-{attempt}",
            std::process::id()
        ));
        match fs::create_dir(&root) {
            Ok(()) => {
                let operation = (|| {
                    let directory = root.join("working-\u{03bc}-\u{3042}");
                    fs::create_dir(&directory)?;
                    test(&fs::canonicalize(directory)?)
                })();
                return combine(
                    operation,
                    fs::remove_dir_all(root).map_err(ProcessError::Io),
                );
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Windows projection directory capacity exceeded",
    )
    .into())
}

#[test]
fn owned_windows_projection_matches_standard_arguments_environment_and_streams(
) -> Result<(), ProcessError> {
    within_directory(|directory| compare(directory, Environment::Explicit))
}

#[test]
fn inherited_windows_environment_matches_standard_merge_and_removal() -> Result<(), ProcessError> {
    within_directory(|directory| {
        let mut parent = Command::new(std::env::current_exe()?);
        parent
            .args(["--exact", PARENT_WORKER, "--nocapture", "--test-threads=1"])
            .env_clear()
            .env(PARENT, "1")
            .env(MODE, "1")
            .env(DIRECTORY, directory)
            .env(VALUE, OsString::from_wide(&WIDE_VALUE))
            .env(CASE_FIRST, "inherited-long-s")
            .env(CASE_SECOND, "inherited-s")
            .env(ASCII_FIRST, "inherited-ascii")
            .env("TMP", directory)
            .env("TEMP", directory)
            .current_dir(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        process::run(&mut parent, Duration::from_secs(50))
    })
}

#[test]
fn inherited_environment_parent() -> Result<(), ProcessError> {
    if std::env::var_os(PARENT).is_none() {
        return Ok(());
    }
    let directory = std::env::var_os(DIRECTORY)
        .ok_or_else(|| io::Error::other("Windows inherited projection directory is missing"))?;
    for environment in [
        Environment::Inherited,
        Environment::SetInherited,
        Environment::RemoveInherited,
    ] {
        compare(Path::new(&directory), environment)?;
    }
    Ok(())
}

#[test]
fn projection_worker() -> io::Result<()> {
    if std::env::var_os(MODE).is_none() {
        return Ok(());
    }
    verify(
        std::env::args_os().any(|argument| argument == OsStr::new(ARGUMENT)),
        "Windows owned argument changed",
    )?;
    verify(
        std::env::var_os(VALUE) == Some(OsString::from_wide(&WIDE_VALUE)),
        "Windows owned environment value changed UTF-16 units",
    )?;
    let directory = std::env::var_os(DIRECTORY)
        .ok_or_else(|| io::Error::other("Windows projection directory is missing"))?;
    verify(
        fs::canonicalize(std::env::current_dir()?)? == Path::new(&directory),
        "Windows owned working directory changed",
    )?;
    let mut environment: Vec<(Vec<u16>, Vec<u16>)> = std::env::vars_os()
        .filter(|(name, _)| name.as_encoded_bytes().starts_with(b"RUSTERISK_"))
        .map(|(name, value)| (name.encode_wide().collect(), value.encode_wide().collect()))
        .collect();
    environment.sort();
    println!(
        "projection aliases={:?}/{:?} environment={environment:?}",
        std::env::var_os(CASE_FIRST),
        std::env::var_os(CASE_SECOND)
    );
    eprintln!("projection stderr");
    Ok(())
}
