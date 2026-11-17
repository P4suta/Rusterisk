use super::checked;
use std::{ffi::OsStr, path::Path};
use xtask::process::{CommandField, CommandRequestError, CommandStream, SupervisedCommand};

fn fill_arguments(request: &mut SupervisedCommand, mut bytes: usize) -> Result<(), String> {
    while bytes > 0 {
        let length = bytes.min(SupervisedCommand::MAX_FIELD_BYTES);
        checked(request.arg("x".repeat(length)))?;
        bytes -= length;
    }
    Ok(())
}

#[test]
fn removing_environment_names_validates_before_mutation_without_private_diagnostics(
) -> Result<(), String> {
    let mut request = checked(SupervisedCommand::new("private-program"))?;
    checked(request.env("PRIVATE_SET", "private-value"))?;
    checked(request.env_remove("PRIVATE_REMOVE"))?;
    let before = format!("{request:?}");
    let oversized = "é".repeat(SupervisedCommand::MAX_FIELD_BYTES / 2 + 1);
    for (name, expected) in [
        (
            "",
            CommandRequestError::Empty(CommandField::EnvironmentName),
        ),
        (
            "private-name\0",
            CommandRequestError::Nul(CommandField::EnvironmentName),
        ),
        ("private=name", CommandRequestError::InvalidEnvironmentName),
        (
            oversized.as_str(),
            CommandRequestError::FieldTooLong(CommandField::EnvironmentName),
        ),
    ] {
        assert_eq!(request.env_remove(name).map(|_| ()), Err(expected));
        assert_eq!(format!("{request:?}"), before);
        assert!(!expected.to_string().contains("private"));
        assert!(!format!("{expected:?}").contains("private"));
    }
    assert!(!before.contains("private"));
    assert!(!before.contains("PRIVATE_"));
    let boundary = "é".repeat(SupervisedCommand::MAX_FIELD_BYTES / 2);
    checked(request.env_remove(&boundary))?;
    let command = request.into_std();
    assert_eq!(command.get_envs().count(), 3);
    assert_eq!(
        command.get_envs().find(|(name, _)| *name == "PRIVATE_SET"),
        Some((OsStr::new("PRIVATE_SET"), Some(OsStr::new("private-value"))))
    );
    assert_eq!(
        command
            .get_envs()
            .find(|(name, _)| *name == "PRIVATE_REMOVE"),
        Some((OsStr::new("PRIVATE_REMOVE"), None))
    );
    assert_eq!(
        command
            .get_envs()
            .find(|(name, _)| *name == boundary.as_str()),
        Some((OsStr::new(&boundary), None))
    );
    Ok(())
}

#[test]
fn removals_share_the_environment_count_and_replace_sets_without_extra_slots() -> Result<(), String>
{
    let mut request = checked(SupervisedCommand::new("p"))?;
    for index in 0..SupervisedCommand::MAX_ENVIRONMENT {
        checked(request.env_remove(format!("K{index}")))?;
    }
    checked(request.env("K0", "private-value"))?;
    checked(request.env_remove("K0"))?;
    checked(request.env_remove("K0"))?;
    checked(request.env("K0", ""))?;
    let before = format!("{request:?}");
    assert_eq!(
        request.env_remove("EXTRA").map(|_| ()),
        Err(CommandRequestError::TooManyEnvironmentVariables)
    );
    assert_eq!(
        request.env("EXTRA", "").map(|_| ()),
        Err(CommandRequestError::TooManyEnvironmentVariables)
    );
    assert_eq!(format!("{request:?}"), before);
    let command = request.into_std();
    assert_eq!(
        command.get_envs().count(),
        SupervisedCommand::MAX_ENVIRONMENT
    );
    assert_eq!(
        command.get_envs().find(|(name, _)| *name == "K0"),
        Some((OsStr::new("K0"), Some(OsStr::new(""))))
    );
    assert!(command
        .get_envs()
        .filter(|(name, _)| *name != "K0")
        .all(|(_, value)| value.is_none()));
    Ok(())
}

#[test]
fn removing_a_set_releases_only_its_value_bytes_and_rejected_removals_are_atomic(
) -> Result<(), String> {
    let mut request = checked(SupervisedCommand::new("p"))?;
    checked(request.env("K", "v"))?;
    fill_arguments(&mut request, SupervisedCommand::MAX_INPUT_BYTES - 3)?;
    let before = format!("{request:?}");
    assert_eq!(
        request.env_remove("NEXT").map(|_| ()),
        Err(CommandRequestError::TotalTooLong)
    );
    assert_eq!(format!("{request:?}"), before);
    checked(request.env_remove("K"))?;
    checked(request.arg("x"))?;
    checked(request.env_remove("K"))?;
    assert_eq!(
        request.env("K", "x").map(|_| ()),
        Err(CommandRequestError::TotalTooLong)
    );
    assert_eq!(
        request.arg("x").map(|_| ()),
        Err(CommandRequestError::TotalTooLong)
    );
    let command = request.into_std();
    assert_eq!(
        command.get_envs().collect::<Vec<_>>(),
        [(OsStr::new("K"), None)]
    );
    assert_eq!(
        command.get_args().map(OsStr::len).sum::<usize>(),
        SupervisedCommand::MAX_INPUT_BYTES - 2
    );
    Ok(())
}

#[test]
fn clearing_environment_releases_both_set_and_remove_count_slots() -> Result<(), String> {
    let mut request = checked(SupervisedCommand::new("p"))?;
    for index in 0..SupervisedCommand::MAX_ENVIRONMENT {
        let name = format!("K{index}");
        if index % 2 == 0 {
            checked(request.env(name, "value"))?;
        } else {
            checked(request.env_remove(name))?;
        }
    }
    checked(request.env_clear())?;
    for index in 0..SupervisedCommand::MAX_ENVIRONMENT {
        checked(request.env(format!("N{index}"), ""))?;
    }
    assert_eq!(
        request.env_remove("EXTRA").map(|_| ()),
        Err(CommandRequestError::TooManyEnvironmentVariables)
    );
    let command = request.into_std();
    assert_eq!(
        command.get_envs().count(),
        SupervisedCommand::MAX_ENVIRONMENT
    );
    assert!(command
        .get_envs()
        .all(|(_, value)| value == Some(OsStr::new(""))));
    Ok(())
}

#[test]
fn clearing_environment_releases_all_explicit_bytes_and_preserves_other_fields(
) -> Result<(), String> {
    let program = "private-program";
    let argument = "private-argument";
    let directory = "private-directory";
    let mut request = checked(SupervisedCommand::new(program))?;
    checked(request.arg(argument))?;
    checked(request.current_dir(directory))?;
    checked(request.env(
        "private-name",
        "x".repeat(SupervisedCommand::MAX_FIELD_BYTES),
    ))?;
    checked(request.env_remove("r".repeat(SupervisedCommand::MAX_FIELD_BYTES)))?;
    request
        .stdout(CommandStream::Inherit)
        .stderr(CommandStream::Null);
    checked(request.env_clear())?;
    checked(request.env_clear())?;
    let debug = format!("{request:?}");
    assert!(!debug.contains("private"));
    assert!(debug.contains("environment: 0"));
    assert!(debug.contains("arguments: 1"));
    assert!(debug.contains("Inherit"));
    assert!(debug.contains("Null"));
    let retained_bytes = program.len() + argument.len() + directory.len();
    fill_arguments(
        &mut request,
        SupervisedCommand::MAX_INPUT_BYTES - retained_bytes,
    )?;
    checked(request.env_clear())?;
    assert_eq!(
        request.env("N", "").map(|_| ()),
        Err(CommandRequestError::TotalTooLong)
    );
    let command = request.into_std();
    assert_eq!(command.get_program(), OsStr::new(program));
    assert_eq!(command.get_args().next(), Some(OsStr::new(argument)));
    assert_eq!(command.get_current_dir(), Some(Path::new(directory)));
    assert_eq!(command.get_envs().count(), 0);
    assert_eq!(
        command.get_args().map(OsStr::len).sum::<usize>(),
        SupervisedCommand::MAX_INPUT_BYTES - program.len() - directory.len()
    );
    Ok(())
}

#[cfg(unix)]
mod native {
    use super::checked;
    use crate::unix::run;
    use std::ffi::OsStr;
    use xtask::process::{CommandStream, SupervisedCommand};

    const MODE: &str = "RUSTERISK_COMMAND_ENV_FIXTURE";
    const PARENT: &str = "RUSTERISK_COMMAND_ENV_PARENT";
    const SET: &str = "RUSTERISK_COMMAND_ENV_SET";

    fn worker(mode: &str) -> Result<SupervisedCommand, String> {
        let mut request = checked(SupervisedCommand::new(checked(std::env::current_exe())?))?;
        for argument in [
            "--exact",
            "environment::native::environment_worker",
            "--nocapture",
        ] {
            checked(request.arg(argument))?;
        }
        checked(request.env(MODE, mode))?;
        request
            .stdout(CommandStream::Inherit)
            .stderr(CommandStream::Inherit);
        Ok(request)
    }

    fn check_parent() -> Result<(), String> {
        if std::env::var_os(PARENT).as_deref() == Some(OsStr::new("parent-value")) {
            Ok(())
        } else {
            Err("outer child did not receive its explicit parent marker".into())
        }
    }

    #[test]
    fn environment_worker() -> Result<(), String> {
        let Some(mode) = std::env::var_os(MODE) else {
            return Ok(());
        };
        match mode.to_str() {
            Some("clear-parent") => {
                check_parent()?;
                let mut child = worker("clear-leaf")?;
                checked(child.env(SET, "before-clear"))?;
                checked(child.env_clear())?;
                checked(child.env(MODE, "clear-leaf"))?;
                checked(run(child))
            }
            Some("clear-leaf") => {
                if std::env::var_os(PARENT).is_some() || std::env::var_os(SET).is_some() {
                    return Err("environment clear retained inherited or explicit values".into());
                }
                Ok(())
            }
            Some("remove-parent") => {
                check_parent()?;
                let mut child = worker("remove-leaf")?;
                checked(child.env(SET, "retained-set"))?;
                checked(child.env_remove(PARENT))?;
                checked(run(child))
            }
            Some("remove-leaf") => {
                if std::env::var_os(PARENT).is_some() {
                    return Err("environment removal retained an inherited value".into());
                }
                if std::env::var_os(SET).as_deref() != Some(OsStr::new("retained-set")) {
                    return Err(
                        "environment removal discarded an independent explicit value".into(),
                    );
                }
                Ok(())
            }
            _ => Err("unexpected environment worker mode".into()),
        }
    }

    #[test]
    fn clearing_environment_blocks_inheritance_and_discards_previous_sets() -> Result<(), String> {
        let mut outer = worker("clear-parent")?;
        checked(outer.env(PARENT, "parent-value"))?;
        checked(run(outer))
    }

    #[test]
    fn removing_environment_blocks_one_inherited_name_without_clearing_sets() -> Result<(), String>
    {
        let mut outer = worker("remove-parent")?;
        checked(outer.env(PARENT, "parent-value"))?;
        checked(run(outer))
    }
}
