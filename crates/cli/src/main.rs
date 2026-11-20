use rusterisk_cli::{Operation, RemoteOptions};
use std::{ffi::OsString, process::ExitCode};

const HELP: &str = "Usage: rusterisk-cli <ping|reload> --server URL --username USER --password-stdin\n\n  ping      Check connectivity to the Asterisk REST Interface.\n  reload    Reload the running server's configuration.\n\nUse HTTPS or loopback HTTP and read the current password from a pipe or file.\n  -h, --help       Print usage.\n  -V, --version    Print the version.";

fn execute(arguments: impl Iterator<Item = OsString>) -> Result<(), (u8, String)> {
    let mut arguments = arguments.peekable();
    let command = arguments.next().unwrap_or_else(|| OsString::from("--help"));
    let command = match command.to_str() {
        Some("ping" | "reload")
            if arguments
                .peek()
                .is_some_and(|option| option == "-h" || option == "--help") =>
        {
            Some("--help")
        }
        name => name,
    };
    let operation = match command {
        Some("-h" | "--help") => {
            println!("{HELP}");
            return Ok(());
        }
        Some("-V" | "--version") => {
            println!("rusterisk-cli {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("ping") => Operation::Ping,
        Some("reload") => Operation::Reload,
        Some(_) | None => return Err((2, "Unknown command. See --help.".to_owned())),
    };
    let options = RemoteOptions::parse(arguments).map_err(|error| (2, error.to_string()))?;
    let message = rusterisk_cli::run(operation, &options)
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
