use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Reload {
        server: String,
        username: String,
    },
    Check {
        directory: PathBuf,
        data_directory: PathBuf,
    },
    Run {
        log_level: tracing::level_filters::LevelFilter,
        directory: PathBuf,
        data_directory: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliError {
    UnknownOption,
    InvalidLogLevel,
    MissingDirectory,
    DuplicateOption,
    Capacity,
    MissingReloadArgument,
    InvalidReloadArgument,
    ReloadProfile(&'static str),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnknownOption => "Unknown option",
            Self::InvalidLogLevel => "Select off, error, warn, info, debug, or trace",
            Self::MissingDirectory => "A directory path is required",
            Self::DuplicateOption => "Duplicate option",
            Self::Capacity => "Command arguments exceed capacity",
            Self::MissingReloadArgument => {
                "Reload requires --password-stdin and either --profile or both --server and --username"
            }
            Self::InvalidReloadArgument => "Invalid reload argument",
            Self::ReloadProfile(message) => message,
        })
    }
}
impl std::error::Error for CliError {}

enum Mode {
    Run,
    Check,
}

impl Command {
    pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, CliError> {
        let arguments: Vec<OsString> = arguments.into_iter().take(17).collect();
        if arguments
            .iter()
            .any(|value| value == "--help" || value == "-h")
        {
            return Ok(Self::Help);
        }
        if arguments
            .iter()
            .any(|value| value == "--version" || value == "-V")
        {
            return Ok(Self::Version);
        }
        if arguments.len() > 16 {
            return Err(CliError::Capacity);
        }
        if arguments.first().is_some_and(|value| value == "reload") {
            return Self::parse_reload(arguments.into_iter().skip(1));
        }
        let mut arguments = arguments.into_iter();
        let mut directory = None;
        let mut data_directory = None;
        let mut mode = Mode::Run;
        let mut log_level = None;
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some(option @ ("--config-dir" | "--data-dir")) => {
                    let destination = if option == "--config-dir" {
                        &mut directory
                    } else {
                        &mut data_directory
                    };
                    if destination.is_some() {
                        return Err(CliError::DuplicateOption);
                    }
                    let path = arguments.next().ok_or(CliError::MissingDirectory)?;
                    if path.is_empty() {
                        return Err(CliError::MissingDirectory);
                    }
                    if path.as_encoded_bytes().len() > 4096 {
                        return Err(CliError::Capacity);
                    }
                    *destination = Some(PathBuf::from(path));
                }
                Some("--log-level") => {
                    if log_level.is_some() {
                        return Err(CliError::DuplicateOption);
                    }
                    let level = arguments.next().ok_or(CliError::InvalidLogLevel)?;
                    let name = level.to_str().ok_or(CliError::InvalidLogLevel)?;
                    log_level = Some(match name {
                        "off" => tracing::level_filters::LevelFilter::OFF,
                        "error" => tracing::level_filters::LevelFilter::ERROR,
                        "warn" => tracing::level_filters::LevelFilter::WARN,
                        "info" => tracing::level_filters::LevelFilter::INFO,
                        "debug" => tracing::level_filters::LevelFilter::DEBUG,
                        "trace" => tracing::level_filters::LevelFilter::TRACE,
                        _ => return Err(CliError::InvalidLogLevel),
                    });
                }
                Some("--check-config") => match mode {
                    Mode::Run => mode = Mode::Check,
                    Mode::Check => return Err(CliError::DuplicateOption),
                },
                Some(_) | None => return Err(CliError::UnknownOption),
            }
        }
        let directory = directory.unwrap_or_else(|| PathBuf::from("config"));
        let data_directory = data_directory.unwrap_or_else(|| PathBuf::from("data"));
        Ok(match mode {
            Mode::Run => Self::Run {
                log_level: log_level.unwrap_or(tracing::level_filters::LevelFilter::INFO),
                directory,
                data_directory,
            },
            Mode::Check => Self::Check {
                directory,
                data_directory,
            },
        })
    }

    fn parse_reload(arguments: impl Iterator<Item = OsString>) -> Result<Self, CliError> {
        let options =
            rusterisk_cli::RemoteOptions::parse(arguments).map_err(|error| match error {
                rusterisk_cli::OptionError::Unknown => CliError::UnknownOption,
                rusterisk_cli::OptionError::Missing => CliError::MissingReloadArgument,
                rusterisk_cli::OptionError::Duplicate => CliError::DuplicateOption,
                rusterisk_cli::OptionError::Capacity => CliError::Capacity,
                rusterisk_cli::OptionError::Invalid => CliError::InvalidReloadArgument,
                rusterisk_cli::OptionError::Profile(message) => CliError::ReloadProfile(message),
            })?;
        Ok(Self::Reload {
            server: options.server,
            username: options.username,
        })
    }
}

pub const HELP: &str = "Rusterisk call server.\n\nUsage: rusterisk [--config-dir PATH] [--data-dir PATH] [--check-config] [--log-level LEVEL]\n       rusterisk reload --server URL --username USER --password-stdin\n\n--config-dir PATH  Read Asterisk configuration from PATH (default: config).\n--data-dir PATH    Store calls and recordings in PATH (default: data).\n--log-level LEVEL  Set the maximum log level (default: info).\n--check-config     Validate configuration and exit.\n--server URL       Address the running server using HTTPS or loopback HTTP.\n--username USER    Authenticate reload with the current server user.\n--password-stdin   Read the current password from nonterminal standard input.\n-h, --help         Print usage and exit.\n-V, --version      Print the version and exit.\n";
