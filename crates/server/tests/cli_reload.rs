use rusterisk_server::cli::{CliError, Command};
use std::ffi::OsString;

fn parse(arguments: &[&str]) -> Result<Command, CliError> {
    Command::parse(arguments.iter().map(OsString::from))
}

#[test]
fn reload_names_the_live_server_and_reads_credentials_separately_from_replacement_settings(
) -> Result<(), CliError> {
    assert_eq!(
        parse(&[
            "reload",
            "--server",
            "https://pbx.example.test:8089",
            "--username",
            "operator",
            "--password-stdin",
        ])?,
        Command::Reload {
            server: "https://pbx.example.test:8089".into(),
            username: "operator".into(),
        }
    );
    assert_eq!(
        parse(&[
            "reload",
            "--password-stdin",
            "--username",
            "operator",
            "--server",
            "http://127.0.0.1:8088",
        ])?,
        Command::Reload {
            server: "http://127.0.0.1:8088".into(),
            username: "operator".into(),
        }
    );
    Ok(())
}

#[test]
fn reload_rejects_password_arguments_startup_paths_and_implicit_destinations() {
    for arguments in [
        vec!["reload"],
        vec!["reload", "--server"],
        vec!["reload", "--username"],
        vec![
            "reload",
            "--server",
            "",
            "--username",
            "operator",
            "--password-stdin",
        ],
        vec![
            "reload",
            "--server",
            "http://127.0.0.1:8088",
            "--username",
            "",
            "--password-stdin",
        ],
        vec![
            "reload",
            "--server",
            "http://127.0.0.1:8088",
            "--username",
            "operator",
        ],
        vec![
            "reload",
            "--server",
            "http://127.0.0.1:8088",
            "--password-stdin",
        ],
    ] {
        assert!(parse(&arguments).is_err(), "{arguments:?}");
    }
    for option in ["--password", "--config-dir", "--data-dir", "--check-config"] {
        assert!(parse(&[
            "reload",
            "--server",
            "http://127.0.0.1:8088",
            "--username",
            "operator",
            "--password-stdin",
            option,
            "untrusted input",
        ])
        .is_err());
    }
}

#[test]
fn reload_preserves_argument_capacity_and_rejects_ambiguous_options() {
    for extra in [
        vec!["--server", "http://127.0.0.1:8089"],
        vec!["--username", "another-user"],
        vec!["--password-stdin"],
        vec!["reload"],
    ] {
        let mut arguments = vec![
            "reload",
            "--server",
            "http://127.0.0.1:8088",
            "--username",
            "operator",
            "--password-stdin",
        ];
        arguments.extend(extra);
        assert!(parse(&arguments).is_err(), "{arguments:?}");
    }
    assert_eq!(parse(&["reload"; 17]), Err(CliError::Capacity));
    for option in ["--server", "--username"] {
        let value = "x".repeat(4097);
        assert_eq!(
            parse(&["reload", option, &value, "--password-stdin"]),
            Err(CliError::Capacity)
        );
    }
}
