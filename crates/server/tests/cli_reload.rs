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
    let cases: &[&[&str]] = &[
        &["reload"],
        &["reload", "--server"],
        &["reload", "--username"],
        &[
            "reload",
            "--server",
            "",
            "--username",
            "operator",
            "--password-stdin",
        ],
        &[
            "reload",
            "--server",
            "http://127.0.0.1:8088",
            "--username",
            "",
            "--password-stdin",
        ],
        &[
            "reload",
            "--server",
            "http://127.0.0.1:8088",
            "--username",
            "operator",
        ],
        &[
            "reload",
            "--server",
            "http://127.0.0.1:8088",
            "--password-stdin",
        ],
    ];
    for arguments in cases {
        assert!(parse(arguments).is_err(), "{arguments:?}");
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
    let cases: &[&[&str]] = &[
        &["--server", "http://127.0.0.1:8089"],
        &["--username", "another-user"],
        &["--password-stdin"],
        &["reload"],
    ];
    for extra in cases {
        let arguments = [
            "reload",
            "--server",
            "http://127.0.0.1:8088",
            "--username",
            "operator",
            "--password-stdin",
        ]
        .into_iter()
        .chain(extra.iter().copied())
        .map(OsString::from);
        assert!(Command::parse(arguments).is_err(), "{extra:?}");
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
