use std::{ffi::OsStr, fmt};
use xtask::process::{CommandField, CommandRequestError, CommandStream, SupervisedCommand};

#[path = "supervised_command/environment.rs"]
mod environment;

#[cfg(windows)]
#[path = "supervised_command/windows_environment.rs"]
mod windows_environment;

#[cfg(unix)]
#[path = "supervised_command/unix.rs"]
mod unix;

fn checked<T>(result: Result<T, impl fmt::Display>) -> Result<T, String> {
    result.map_err(|error| error.to_string())
}

#[test]
fn invalid_fields_return_typed_errors_without_echoing_values() -> Result<(), String> {
    assert!(matches!(
        SupervisedCommand::new(""),
        Err(CommandRequestError::Empty(CommandField::Program))
    ));
    assert!(matches!(
        SupervisedCommand::new("private-program\0"),
        Err(CommandRequestError::Nul(CommandField::Program))
    ));
    let oversized = "private".repeat(SupervisedCommand::MAX_FIELD_BYTES);
    assert!(matches!(
        SupervisedCommand::new(&oversized),
        Err(CommandRequestError::FieldTooLong(CommandField::Program))
    ));
    let mut request = checked(SupervisedCommand::new("private-program"))?;
    for (result, expected) in [
        (
            request.arg("private-argument\0").map(|_| ()),
            CommandRequestError::Nul(CommandField::Argument),
        ),
        (
            request.env("", "private-value").map(|_| ()),
            CommandRequestError::Empty(CommandField::EnvironmentName),
        ),
        (
            request.env("private=name", "private-value").map(|_| ()),
            CommandRequestError::InvalidEnvironmentName,
        ),
        (
            request.env("private-name\0", "private-value").map(|_| ()),
            CommandRequestError::Nul(CommandField::EnvironmentName),
        ),
        (
            request.env("private-name", "private-value\0").map(|_| ()),
            CommandRequestError::Nul(CommandField::EnvironmentValue),
        ),
        (
            request.current_dir("").map(|_| ()),
            CommandRequestError::Empty(CommandField::Directory),
        ),
        (
            request.current_dir("private-directory\0").map(|_| ()),
            CommandRequestError::Nul(CommandField::Directory),
        ),
    ] {
        assert_eq!(result, Err(expected));
        assert!(!expected.to_string().contains("private"));
        assert!(!format!("{expected:?}").contains("private"));
    }
    checked(request.arg(""))?;
    checked(request.env("EMPTY", ""))?;
    let command = request.into_std();
    assert_eq!(command.get_args().collect::<Vec<_>>(), [OsStr::new("")]);
    assert_eq!(
        command.get_envs().collect::<Vec<_>>(),
        [(OsStr::new("EMPTY"), Some(OsStr::new("")))]
    );
    assert_eq!(command.get_current_dir(), None);
    Ok(())
}

#[test]
fn every_field_has_an_individual_encoded_byte_limit() -> Result<(), String> {
    let boundary = "x".repeat(SupervisedCommand::MAX_FIELD_BYTES);
    let oversized = format!("{boundary}x");
    checked(SupervisedCommand::new(&boundary))?;
    checked(SupervisedCommand::new(
        "é".repeat(SupervisedCommand::MAX_FIELD_BYTES / 2),
    ))?;
    assert!(matches!(
        SupervisedCommand::new("é".repeat(SupervisedCommand::MAX_FIELD_BYTES / 2 + 1)),
        Err(CommandRequestError::FieldTooLong(CommandField::Program))
    ));
    for field in [
        CommandField::Argument,
        CommandField::EnvironmentName,
        CommandField::EnvironmentValue,
        CommandField::Directory,
    ] {
        let mut request = checked(SupervisedCommand::new("p"))?;
        let valid = match field {
            CommandField::Argument => request.arg(&boundary),
            CommandField::EnvironmentName => request.env(&boundary, ""),
            CommandField::EnvironmentValue => request.env("K", &boundary),
            CommandField::Directory => request.current_dir(&boundary),
            CommandField::Program => return Err("unexpected program field".into()),
        };
        checked(valid)?;
        let rejected = match field {
            CommandField::Argument => request.arg(&oversized),
            CommandField::EnvironmentName => request.env(&oversized, ""),
            CommandField::EnvironmentValue => request.env("K", &oversized),
            CommandField::Directory => request.current_dir(&oversized),
            CommandField::Program => return Err("unexpected program field".into()),
        };
        assert_eq!(
            rejected.map(|_| ()),
            Err(CommandRequestError::FieldTooLong(field))
        );
    }
    Ok(())
}

#[test]
fn argument_and_environment_counts_are_bounded() -> Result<(), String> {
    let mut arguments = checked(SupervisedCommand::new("p"))?;
    for _ in 0..SupervisedCommand::MAX_ARGUMENTS {
        checked(arguments.arg(""))?;
    }
    assert_eq!(
        arguments.arg("").map(|_| ()),
        Err(CommandRequestError::TooManyArguments)
    );
    assert_eq!(
        arguments.into_std().get_args().count(),
        SupervisedCommand::MAX_ARGUMENTS
    );
    let mut environment = checked(SupervisedCommand::new("p"))?;
    for index in 0..SupervisedCommand::MAX_ENVIRONMENT {
        checked(environment.env(format!("K{index}"), ""))?;
    }
    checked(environment.env("K0", "replacement"))?;
    assert_eq!(
        environment.env("EXTRA", "").map(|_| ()),
        Err(CommandRequestError::TooManyEnvironmentVariables)
    );
    let command = environment.into_std();
    assert_eq!(
        command.get_envs().count(),
        SupervisedCommand::MAX_ENVIRONMENT
    );
    assert_eq!(
        command.get_envs().find(|(name, _)| *name == "K0"),
        Some((OsStr::new("K0"), Some(OsStr::new("replacement"))))
    );
    Ok(())
}

#[test]
fn diagnostics_show_counts_and_stream_modes_without_private_values() -> Result<(), String> {
    let mut request = checked(SupervisedCommand::new("private-program"))?;
    checked(request.arg("private-argument"))?;
    checked(request.env("private-name", "private-value"))?;
    checked(request.current_dir("private-directory"))?;
    request
        .stdout(CommandStream::Inherit)
        .stderr(CommandStream::Null);
    let debug = format!("{request:?}");
    assert!(!debug.contains("private"));
    assert!(debug.contains("arguments: 1"));
    assert!(debug.contains("environment: 1"));
    assert!(debug.contains("Inherit"));
    assert!(debug.contains("Null"));
    Ok(())
}

#[test]
fn rejected_mutations_preserve_all_existing_request_values() -> Result<(), String> {
    let mut request = checked(SupervisedCommand::new("p"))?;
    checked(request.env("K", "v"))?;
    checked(request.current_dir("."))?;
    let mut remaining = SupervisedCommand::MAX_INPUT_BYTES - 4;
    while remaining > 0 {
        let bytes = remaining.min(SupervisedCommand::MAX_FIELD_BYTES);
        checked(request.arg("x".repeat(bytes)))?;
        remaining -= bytes;
    }
    let before = format!("{request:?}");
    assert_eq!(
        request.arg("x").map(|_| ()),
        Err(CommandRequestError::TotalTooLong)
    );
    assert_eq!(
        request.env("K", "vv").map(|_| ()),
        Err(CommandRequestError::TotalTooLong)
    );
    assert_eq!(
        request.current_dir("..").map(|_| ()),
        Err(CommandRequestError::TotalTooLong)
    );
    assert_eq!(
        request.arg("\0").map(|_| ()),
        Err(CommandRequestError::Nul(CommandField::Argument))
    );
    assert_eq!(before, format!("{request:?}"));
    let command = request.into_std();
    assert_eq!(command.get_program(), OsStr::new("p"));
    assert_eq!(command.get_current_dir(), Some(std::path::Path::new(".")));
    assert_eq!(
        command.get_envs().collect::<Vec<_>>(),
        [(OsStr::new("K"), Some(OsStr::new("v")))]
    );
    assert_eq!(
        command.get_args().map(OsStr::len).sum::<usize>(),
        SupervisedCommand::MAX_INPUT_BYTES - 4
    );
    Ok(())
}

#[test]
fn replacing_environment_and_directory_values_releases_their_byte_budget() -> Result<(), String> {
    let mut request = checked(SupervisedCommand::new("p"))?;
    checked(request.env("K", "value"))?;
    checked(request.current_dir("long-directory"))?;
    checked(request.env("K", ""))?;
    checked(request.current_dir("."))?;
    let mut remaining = SupervisedCommand::MAX_INPUT_BYTES - 3;
    while remaining > 0 {
        let bytes = remaining.min(SupervisedCommand::MAX_FIELD_BYTES);
        checked(request.arg("x".repeat(bytes)))?;
        remaining -= bytes;
    }
    let command = request.into_std();
    assert_eq!(
        command.get_envs().collect::<Vec<_>>(),
        [(OsStr::new("K"), Some(OsStr::new("")))]
    );
    assert_eq!(command.get_current_dir(), Some(std::path::Path::new(".")));
    Ok(())
}
