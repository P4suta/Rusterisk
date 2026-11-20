mod http;
mod options;

pub use options::{OptionError, RemoteOptions};
use std::io::{self, IsTerminal, Read};

#[derive(Clone, Copy)]
pub enum Operation {
    Ping,
    Reload,
}

#[derive(Debug)]
pub enum Error {
    Input(&'static str),
    ConnectionSettings(&'static str),
    Credential(io::Error),
    Runtime(io::Error),
    Transport(reqwest::Error),
    Response(&'static str),
    Remote(reqwest::StatusCode, String),
}

impl Error {
    pub const fn exit_status(&self) -> u8 {
        match self {
            Self::Input(_) | Self::Credential(_) => 2,
            Self::ConnectionSettings(_)
            | Self::Runtime(_)
            | Self::Transport(_)
            | Self::Response(_)
            | Self::Remote(_, _) => 1,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(message) | Self::ConnectionSettings(message) | Self::Response(message) => {
                f.write_str(message)
            }
            Self::Runtime(error) | Self::Credential(error) => error.fmt(f),
            Self::Transport(error) => error.fmt(f),
            Self::Remote(status, message) => write!(f, "HTTP {}: {message}", status.as_u16()),
        }
    }
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Self::Transport(error.without_url())
    }
}

fn password(input: impl Read) -> Result<String, Error> {
    let mut bytes = Vec::with_capacity(259);
    input
        .take(259)
        .read_to_end(&mut bytes)
        .map_err(Error::Credential)?;
    if bytes.ends_with(b"\r\n") {
        bytes.truncate(bytes.len() - 2);
    } else if bytes.ends_with(b"\n") {
        bytes.pop();
    }
    if bytes.is_empty() || bytes.len() > 256 || bytes.contains(&b'\n') || bytes.contains(&b'\r') {
        return Err(Error::Input(
            "Standard input must contain one password of 1 to 256 UTF-8 bytes",
        ));
    }
    String::from_utf8(bytes).map_err(|_| Error::Input("The password must be valid UTF-8"))
}

pub fn run(operation: Operation, options: &RemoteOptions) -> Result<&'static str, Error> {
    let input = io::stdin();
    if input.is_terminal() {
        return Err(Error::Input("Use --password-stdin with a pipe or file"));
    }
    let password = password(input.lock())?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(4)
        .build()
        .map_err(Error::Runtime)?
        .block_on(http::request(operation, options, &password))
}
