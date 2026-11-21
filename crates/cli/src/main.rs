use rusterisk_cli::{Inspection, Operation, RemoteOptions};
use std::{ffi::OsString, process::ExitCode};

const HELP: &str = "Usage: rusterisk-cli COMMAND [TARGET] --server URL --username USER --password-stdin\n\n  ping                    Check connectivity.\n  reload                  Reload the server configuration.\n  status                  Show the server version and start time.\n  endpoints               List endpoint routes.\n  endpoint TECH/RESOURCE  Inspect one endpoint route.\n  channels                List active call legs and their bridges.\n  channel ID              Inspect one call leg and its bridges.\n\n  --json          Print inspection results as JSON.\n  -h, --help      Print usage.\n  -V, --version   Print the version.\n\nUse HTTPS or loopback HTTP and read the password from a pipe or file.";

type UsageError = (u8, String);

fn segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !matches!(value, "." | "..")
        && !value.contains('/')
        && !value.chars().any(char::is_control)
}

fn target(arguments: &mut impl Iterator<Item = OsString>) -> Result<String, UsageError> {
    arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.starts_with("--") && value.len() <= 513)
        .ok_or_else(|| {
            (
                2,
                "Supply the resource identifier after the command".to_owned(),
            )
        })
}

enum Command {
    Control(Operation),
    Inspect(Inspection),
}

fn execute(arguments: impl Iterator<Item = OsString>) -> Result<(), UsageError> {
    let mut arguments = arguments.peekable();
    let command = arguments.next().unwrap_or_else(|| OsString::from("--help"));
    let name = command
        .to_str()
        .ok_or_else(|| (2, "Invalid command".to_owned()))?;
    if matches!(name, "-h" | "--help")
        || matches!(
            name,
            "ping" | "reload" | "status" | "endpoints" | "endpoint" | "channels" | "channel"
        ) && arguments
            .peek()
            .is_some_and(|option| option == "-h" || option == "--help")
    {
        println!("{HELP}");
        return Ok(());
    }
    let command = match name {
        "-V" | "--version" => {
            println!("rusterisk-cli {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        "ping" => Command::Control(Operation::Ping),
        "reload" => Command::Control(Operation::Reload),
        "status" => Command::Inspect(Inspection::Status),
        "endpoints" => Command::Inspect(Inspection::Endpoints),
        "channels" => Command::Inspect(Inspection::Channels),
        "endpoint" => {
            let id = target(&mut arguments)?;
            let (technology, resource) = id
                .split_once('/')
                .filter(|(t, r)| segment(t) && segment(r))
                .ok_or_else(|| (2, "Supply the endpoint as TECH/RESOURCE".to_owned()))?;
            Command::Inspect(Inspection::Endpoint {
                technology: technology.to_owned(),
                resource: resource.to_owned(),
            })
        }
        "channel" => {
            let id = target(&mut arguments)?;
            if !segment(&id) {
                return Err((2, "Invalid channel identifier".to_owned()));
            }
            Command::Inspect(Inspection::Channel(id))
        }
        _ => return Err((2, "Unknown command. See --help.".to_owned())),
    };
    let (options, json) =
        RemoteOptions::parse_with_output(arguments).map_err(|error| (2, error.to_string()))?;
    let message = match command {
        Command::Control(_) if json => {
            return Err((2, "Use --json with an inspection command".to_owned()))
        }
        Command::Control(operation) => rusterisk_cli::run(operation, &options).map(str::to_owned),
        Command::Inspect(operation) => rusterisk_cli::inspect(&operation, &options, json),
    }
    .map_err(|error| (error.exit_status(), error.to_string()))?;
    println!("{message}");
    Ok(())
}

fn main() -> ExitCode {
    match execute(std::env::args_os().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err((status, message)) => {
            eprintln!("rusterisk-cli: {message}");
            ExitCode::from(status)
        }
    }
}
