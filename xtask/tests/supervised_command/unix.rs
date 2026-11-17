use super::checked;
use std::{
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::OwnedFd,
        unix::{ffi::OsStringExt, net::UnixStream},
    },
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use xtask::process::{CommandStream, CommandSupervisor, ProcessError, SupervisedCommand};

const MODE: &str = "RUSTERISK_COMMAND_REQUEST_FIXTURE";
const ARGUMENT: &str = "argument with spaces ; $(literal) \"quote\"";

fn request(mode: &str) -> Result<SupervisedCommand, String> {
    let mut request = checked(SupervisedCommand::new(checked(std::env::current_exe())?))?;
    for argument in ["--exact", "unix::command_request_fixture", "--nocapture"] {
        checked(request.arg(argument))?;
    }
    checked(request.env(MODE, mode))?;
    Ok(request)
}

fn within_directory(operation: impl FnOnce(&Path) -> Result<(), String>) -> Result<(), String> {
    let stamp = checked(SystemTime::now().duration_since(UNIX_EPOCH))?.as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "rusterisk-command-request-{}-{stamp}",
        std::process::id()
    ));
    checked(std::fs::create_dir(&directory))?;
    let result = checked(std::fs::canonicalize(&directory)).and_then(|path| operation(&path));
    match (result, std::fs::remove_dir_all(&directory)) {
        (result, Ok(())) => result,
        (Ok(()), Err(error)) => Err(error.to_string()),
        (Err(operation), Err(cleanup)) => {
            Err(format!("{operation}; fixture cleanup failed: {cleanup}"))
        }
    }
}

pub(super) fn run(request: SupervisedCommand) -> Result<(), ProcessError> {
    let mut supervisor = CommandSupervisor::new()?;
    let result = supervisor.run(&mut request.into_std(), Duration::from_secs(5));
    match (result, supervisor.finish()) {
        (result, Ok(())) => result,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(ProcessError::Cleanup {
            operation: operation.to_string(),
            cleanup: cleanup.to_string(),
        }),
    }
}

#[test]
fn command_request_fixture() -> Result<(), String> {
    let Some(mode) = std::env::var_os(MODE) else {
        return Ok(());
    };
    match mode.to_str() {
        Some("capture") => {
            if !std::env::args().any(|argument| argument == ARGUMENT) {
                return Err("fixture argument was not preserved".into());
            }
            if checked(std::env::var("RUSTERISK_COMMAND_VALUE"))? != "received=value" {
                return Err("fixture environment was not preserved".into());
            }
            let expected = std::env::var_os("RUSTERISK_COMMAND_DIRECTORY")
                .ok_or("missing fixture directory")?;
            if checked(std::env::current_dir())? != Path::new(&expected) {
                return Err("fixture directory was not preserved".into());
            }
            let mut input = [0; 1];
            if checked(io::stdin().read(&mut input))? != 0 {
                return Err("fixture stdin was not null".into());
            }
            checked(writeln!(io::stdout(), "received stdout"))?;
            checked(writeln!(io::stderr(), "received stderr"))?;
            Ok(())
        }
        Some("failure") => Err("requested fixture failure".into()),
        Some("inherit-parent" | "null-parent") => {
            let mut child = request("stream-leaf")?;
            if mode == "inherit-parent" {
                child
                    .stdout(CommandStream::Inherit)
                    .stderr(CommandStream::Inherit);
            }
            checked(run(child))
        }
        Some("stream-leaf") => {
            checked(writeln!(io::stdout(), "stream stdout marker"))?;
            checked(writeln!(io::stderr(), "stream stderr marker"))?;
            Ok(())
        }
        _ => Err("unexpected fixture mode".into()),
    }
}

#[test]
fn the_unix_adapter_delivers_arguments_environment_directory_and_owned_output_files(
) -> Result<(), String> {
    within_directory(|directory| {
        let stdout = directory.join("stdout");
        let stderr = directory.join("stderr");
        let mut request = request("capture")?;
        checked(request.arg("--skip"))?;
        checked(request.arg(ARGUMENT))?;
        checked(request.env("RUSTERISK_COMMAND_VALUE", "received=value"))?;
        checked(request.env("RUSTERISK_COMMAND_DIRECTORY", directory))?;
        checked(request.current_dir(directory))?;
        request
            .stdout(CommandStream::File(checked(File::create(&stdout))?))
            .stderr(CommandStream::File(checked(File::create(&stderr))?));
        checked(run(request))?;
        assert!(checked(std::fs::read_to_string(stdout))?.contains("received stdout"));
        assert_eq!(
            checked(std::fs::read_to_string(stderr))?,
            "received stderr\n"
        );
        Ok(())
    })
}

#[test]
fn inherit_and_null_streams_have_distinct_native_behavior() -> Result<(), String> {
    within_directory(|directory| {
        for (mode, inherited) in [("inherit-parent", true), ("null-parent", false)] {
            let stdout = directory.join(format!("{mode}-stdout"));
            let stderr = directory.join(format!("{mode}-stderr"));
            let mut request = request(mode)?;
            request
                .stdout(CommandStream::File(checked(File::create(&stdout))?))
                .stderr(CommandStream::File(checked(File::create(&stderr))?));
            checked(run(request))?;
            assert_eq!(
                checked(std::fs::read_to_string(stdout))?.contains("stream stdout marker"),
                inherited
            );
            assert_eq!(
                checked(std::fs::read_to_string(stderr))?.contains("stream stderr marker"),
                inherited
            );
        }
        Ok(())
    })
}

#[test]
fn the_unix_adapter_preserves_non_unicode_native_values() -> Result<(), String> {
    let value = std::ffi::OsString::from_vec([b'a', 0xff].to_vec());
    let mut request = checked(SupervisedCommand::new(&value))?;
    checked(request.arg(&value))?;
    checked(request.env(&value, &value))?;
    checked(request.current_dir(&value))?;
    let command = request.into_std();
    assert_eq!(command.get_program(), &value);
    assert_eq!(command.get_args().collect::<Vec<_>>(), [value.as_os_str()]);
    assert_eq!(
        command.get_envs().collect::<Vec<_>>(),
        [(value.as_os_str(), Some(value.as_os_str()))]
    );
    assert_eq!(command.get_current_dir(), Some(Path::new(&value)));
    Ok(())
}

fn output_pair() -> Result<(UnixStream, CommandStream), String> {
    let (reader, writer) = checked(UnixStream::pair())?;
    checked(reader.set_read_timeout(Some(Duration::from_millis(250))))?;
    Ok((
        reader,
        CommandStream::File(File::from(OwnedFd::from(writer))),
    ))
}

fn assert_closed(mut reader: UnixStream) -> Result<(), String> {
    let mut bytes = [0; 1];
    assert_eq!(checked(reader.read(&mut bytes))?, 0);
    Ok(())
}

#[test]
fn replacing_or_dropping_owned_streams_closes_their_files() -> Result<(), String> {
    let (stdout, output) = output_pair()?;
    let (stderr, diagnostic) = output_pair()?;
    let mut request = checked(SupervisedCommand::new("unused-program"))?;
    request.stdout(output).stderr(diagnostic);
    assert_eq!(format!("{request:?}").matches("File").count(), 2);
    request.stdout(CommandStream::Null);
    assert_closed(stdout)?;
    drop(request);
    assert_closed(stderr)
}

fn received_then_closed(reader: UnixStream, marker: &str) -> Result<(), String> {
    let mut bytes = Vec::new();
    checked(reader.take(1024).read_to_end(&mut bytes))?;
    assert!(bytes.len() < 1024);
    assert!(checked(String::from_utf8(bytes))?.contains(marker));
    Ok(())
}

#[test]
fn successful_children_release_both_owned_output_streams() -> Result<(), String> {
    let (stdout, output) = output_pair()?;
    let (stderr, diagnostic) = output_pair()?;
    let mut request = request("stream-leaf")?;
    request.stdout(output).stderr(diagnostic);
    checked(run(request))?;
    received_then_closed(stdout, "stream stdout marker")?;
    received_then_closed(stderr, "stream stderr marker")
}

#[test]
fn nonzero_exit_and_spawn_failures_are_preserved_and_owned_files_close() -> Result<(), String> {
    assert!(matches!(
        run(request("failure")?),
        Err(ProcessError::Status(_))
    ));
    within_directory(|directory| {
        let (stdout, output) = output_pair()?;
        let (stderr, diagnostic) = output_pair()?;
        let mut missing = checked(SupervisedCommand::new(directory.join("missing-program")))?;
        missing.stdout(output).stderr(diagnostic);
        assert!(
            matches!(run(missing), Err(ProcessError::Io(error)) if error.kind() == io::ErrorKind::NotFound)
        );
        assert_closed(stdout)?;
        assert_closed(stderr)?;
        let mut request = request("stream-leaf")?;
        checked(request.current_dir(directory.join("missing-directory")))?;
        assert!(
            matches!(run(request), Err(ProcessError::Io(error)) if error.kind() == io::ErrorKind::NotFound)
        );
        Ok(())
    })
}
