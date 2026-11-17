use rusterisk_server::cli::{CliError, Command};
use std::ffi::OsString;
use std::path::PathBuf;

fn parse(arguments: &[&str]) -> Result<Command, CliError> {
    Command::parse(arguments.iter().map(OsString::from))
}

#[test]
fn informational_modes_do_not_require_configuration_and_validation_is_distinct_from_startup(
) -> Result<(), CliError> {
    assert_eq!(
        parse(&[])?,
        Command::Run {
            directory: PathBuf::from("config"),
            data_directory: PathBuf::from("data"),
        }
    );
    assert_eq!(parse(&["--unknown", "--help"])?, Command::Help);
    assert_eq!(parse(&["--check-config", "--version"])?, Command::Version);
    assert_eq!(
        parse(&["--config-dir", "settings", "--check-config"])?,
        Command::Check {
            directory: PathBuf::from("settings"),
            data_directory: PathBuf::from("data"),
        }
    );
    Ok(())
}

#[test]
fn persistence_paths_are_explicit_bounded_and_independent_of_configuration() -> Result<(), CliError>
{
    assert_eq!(
        parse(&[
            "--check-config",
            "--config-dir",
            "settings",
            "--data-dir",
            "private calls",
        ])?,
        Command::Check {
            directory: PathBuf::from("settings"),
            data_directory: PathBuf::from("private calls"),
        }
    );
    assert_eq!(
        parse(&["--config-dir", "settings", "--data-dir", "private calls"])?,
        Command::Run {
            directory: PathBuf::from("settings"),
            data_directory: PathBuf::from("private calls"),
        }
    );
    assert_eq!(parse(&["--data-dir"]), Err(CliError::MissingDirectory));
    assert_eq!(parse(&["--data-dir", ""]), Err(CliError::MissingDirectory));
    assert_eq!(
        parse(&["--data-dir", "a", "--data-dir", "b"]),
        Err(CliError::DuplicateOption)
    );
    assert_eq!(
        parse(&["--data-dir", &"a".repeat(4097)]),
        Err(CliError::Capacity)
    );
    Ok(())
}

#[test]
fn invalid_or_unbounded_arguments_have_explicit_failures() {
    assert_eq!(parse(&["--unknown"]), Err(CliError::UnknownOption));
    assert_eq!(parse(&["--config-dir"]), Err(CliError::MissingDirectory));
    assert_eq!(
        parse(&["--config-dir", ""]),
        Err(CliError::MissingDirectory)
    );
    assert_eq!(
        parse(&["--config-dir", "a", "--config-dir", "b"]),
        Err(CliError::DuplicateOption)
    );
    assert_eq!(
        parse(&["--check-config", "--check-config"]),
        Err(CliError::DuplicateOption)
    );
    assert_eq!(parse(&["--check-config"; 17]), Err(CliError::Capacity));
    assert_eq!(
        parse(&["--config-dir", &"a".repeat(4097)]),
        Err(CliError::Capacity)
    );
}
