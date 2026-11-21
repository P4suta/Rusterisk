use rusterisk_cli::{Doctor, Inspection, Operation, RemoteOptions, Watch};
use std::{ffi::OsString, process::ExitCode};

const HELP: &str = concat!(
    "Usage: rusterisk-cli COMMAND [TARGET] (--server URL --username USER | --profile NAME) --password-stdin\n",
    "\n",
    "  profiles                List local server profiles.\n",
    "  ping                    Check connectivity.\n",
    "  reload                  Reload the server configuration.\n",
    "  status                  Show the server version and start time.\n",
    "  endpoints               List endpoint routes.\n",
    "  endpoint TECH/RESOURCE  Inspect one endpoint route.\n",
    "  channels                List active call legs and their bridges.\n",
    "  channel ID              Inspect one call leg and its bridges.\n",
    "  doctor extension CONTEXT NUMBER  Diagnose a dialplan route.\n",
    "  doctor channel ID       Diagnose signaling and sample packet traffic.\n",
    "  watch channel ID        Follow one call leg until it ends.\n",
    "  watch endpoint TECH/RESOURCE  Follow an endpoint and its call legs.\n",
    "\n",
    "  test-call PJSIP/NAME    Call a nominated echo endpoint and verify returned audio.\n",
    "  --deadline SECONDS      Bound a test call to 1 through 120 seconds.\n",
    "\n",
    "  --json          Print inspection results as JSON or watch records as NDJSON.\n",
    "  -h, --help      Print usage.\n",
    "  -V, --version   Print the version.\n",
    "\n",
    "Use HTTPS or loopback HTTP and read the password from a pipe or file.",
);

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
    Watch(Watch),
    Diagnose(Doctor),
}

fn endpoint(
    arguments: &mut impl Iterator<Item = OsString>,
) -> Result<(String, String), UsageError> {
    let id = target(arguments)?;
    let (technology, resource) = id
        .split_once('/')
        .filter(|(t, r)| segment(t) && segment(r))
        .ok_or_else(|| (2, "Supply the endpoint as TECH/RESOURCE".to_owned()))?;
    Ok((technology.to_owned(), resource.to_owned()))
}

fn channel(arguments: &mut impl Iterator<Item = OsString>) -> Result<String, UsageError> {
    let id = target(arguments)?;
    if !segment(&id) {
        return Err((2, "Invalid channel identifier".to_owned()));
    }
    Ok(id)
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
            "ping"
                | "reload"
                | "status"
                | "endpoints"
                | "endpoint"
                | "channels"
                | "channel"
                | "profiles"
                | "watch"
                | "doctor"
                | "test-call"
        ) && arguments
            .peek()
            .is_some_and(|option| option == "-h" || option == "--help")
    {
        println!("{HELP}");
        return Ok(());
    }
    let command = match name {
        "test-call" => {
            let selected = target(&mut arguments)?;
            let resource = selected
                .strip_prefix("PJSIP/")
                .filter(|r| segment(r) && !r.contains('@'))
                .ok_or_else(|| (2, "Select a configured PJSIP echo endpoint".to_owned()))?;
            let (deadline, remote) = test_options(arguments)?;
            let (options, json) = RemoteOptions::parse_with_output(remote.into_iter())
                .map_err(|error| (2, error.to_string()))?;
            let (message, verified) = rusterisk_cli::test_call(&options, resource, deadline, json)
                .map_err(|e| (e.exit_status(), e.to_string()))?;
            println!("{message}");
            if !verified {
                return Err((
                    1,
                    "The test call did not verify the returned signal".to_owned(),
                ));
            }
            return Ok(());
        }
        "profiles" => {
            if arguments.next().is_some() {
                return Err((2, "The profiles command takes no arguments".to_owned()));
            }
            for name in rusterisk_cli::profile_names().map_err(|error| (2, error.to_string()))? {
                println!("{name}");
            }
            return Ok(());
        }
        "-V" | "--version" => {
            println!("rusterisk-cli {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        "ping" => Command::Control(Operation::Ping),
        "reload" => Command::Control(Operation::Reload),
        "status" => Command::Inspect(Inspection::Status),
        "doctor" => {
            let resource = target(&mut arguments)?;
            let first = target(&mut arguments)?;
            if !segment(&first) {
                return Err((2, "Invalid diagnosis target".to_owned()));
            }
            match resource.as_str() {
                "channel" => Command::Diagnose(Doctor::Channel(first)),
                "extension" => {
                    let extension = target(&mut arguments)?;
                    if !segment(&extension) {
                        return Err((2, "Invalid extension".to_owned()));
                    }
                    Command::Diagnose(Doctor::Extension {
                        context: first,
                        extension,
                    })
                }
                _ => return Err((2, "Select a channel or an extension to diagnose".to_owned())),
            }
        }
        "endpoints" => Command::Inspect(Inspection::Endpoints),
        "channels" => Command::Inspect(Inspection::Channels),
        "endpoint" => {
            let (technology, resource) = endpoint(&mut arguments)?;
            Command::Inspect(Inspection::Endpoint {
                technology,
                resource,
            })
        }
        "channel" => Command::Inspect(Inspection::Channel(channel(&mut arguments)?)),
        "watch" => match target(&mut arguments)?.as_str() {
            "channel" => Command::Watch(Watch::Channel(channel(&mut arguments)?)),
            "endpoint" => {
                let (technology, resource) = endpoint(&mut arguments)?;
                Command::Watch(Watch::Endpoint {
                    technology,
                    resource,
                })
            }
            _ => return Err((2, "Select a channel or endpoint to watch".to_owned())),
        },
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
        Command::Watch(target) => {
            return rusterisk_cli::watch(&target, &options, json)
                .map_err(|error| (error.exit_status(), error.to_string()))
        }
        Command::Diagnose(operation) => rusterisk_cli::diagnose(&operation, &options, json),
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

fn test_options(
    mut args: impl Iterator<Item = OsString>,
) -> Result<(u16, Vec<OsString>), UsageError> {
    let mut deadline = None;
    let mut remote = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--deadline" {
            if deadline.is_some() {
                return Err((2, "Duplicate test call deadline".to_owned()));
            }
            let value = target(&mut args)?
                .parse::<u16>()
                .ok()
                .filter(|v| (1..=120).contains(v))
                .ok_or_else(|| (2, "Supply a deadline of 1 to 120 seconds".to_owned()))?;
            deadline = Some(value);
        } else {
            let value = matches!(arg.to_str(), Some("--server" | "--username" | "--profile"));
            remote.push(arg);
            if value {
                remote.push(
                    args.next()
                        .ok_or_else(|| (2, "Missing remote option value".to_owned()))?,
                );
            }
            if remote.len() > 12 {
                return Err((2, "Too many test call options".to_owned()));
            }
        }
    }
    Ok((deadline.unwrap_or(10), remote))
}
