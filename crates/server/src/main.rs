use rusterisk_config::{Config, ConfigError};
use rusterisk_server::cli::{CliError, Command};
use rusterisk_server::runtime::Server;
use rusterisk_server::ControlError;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

enum AppError {
    Arguments(CliError),
    Config(ConfigError),
    Control(ControlError),
    Runtime(io::Error),
    Logging(tracing::subscriber::SetGlobalDefaultError),
    Shutdown {
        signal: io::Error,
        cleanup: Option<ControlError>,
    },
}

impl AppError {
    const fn status(&self) -> u8 {
        match self {
            Self::Arguments(_) | Self::Config(_) => 2,
            Self::Control(_) | Self::Runtime(_) | Self::Logging(_) | Self::Shutdown { .. } => 1,
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Arguments(error) => write!(f, "{error}"),
            Self::Config(error) => write!(f, "{error}"),
            Self::Control(error) => write!(f, "{error}"),
            Self::Runtime(error) => write!(f, "{error}"),
            Self::Logging(error) => write!(f, "{error}"),
            Self::Shutdown { signal, cleanup } => {
                write!(f, "Shutdown signal failed: {signal}")?;
                if let Some(error) = cleanup {
                    write!(f, "; cleanup failed: {error}")?;
                }
                Ok(())
            }
        }
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, AppError> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(4)
        .thread_name("rusterisk")
        .enable_all()
        .build()
        .map_err(AppError::Runtime)
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<(), io::Error> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        result = terminate.recv() => result.ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "Termination signal channel closed")),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<(), io::Error> {
    tokio::signal::ctrl_c().await
}

async fn start(config: Config, data_directory: PathBuf) -> Result<(), AppError> {
    let server = Server::bind(config, 64, data_directory)
        .await
        .map_err(AppError::Control)?;
    let controller = server.controller();
    tracing::info!(http = %server.local_addr().map_err(AppError::Control)?, sip = ?server.sip_listeners().map_err(AppError::Control)?, "server ready");
    let serving = server.run();
    tokio::pin!(serving);
    tokio::select! {
        result = &mut serving => result.map_err(AppError::Control),
        signal = shutdown_signal() => {
            controller.shutdown();
            let cleanup = serving.await;
            match (signal, cleanup) {
                (Ok(()), Ok(())) => Ok(()),
                (Ok(()), Err(error)) => Err(AppError::Control(error)),
                (Err(signal), Ok(())) => Err(AppError::Shutdown { signal, cleanup: None }),
                (Err(signal), Err(error)) => Err(AppError::Shutdown { signal, cleanup: Some(error) }),
            }
        }
    }
}

fn execute(command: Command) -> Result<(), AppError> {
    match command {
        Command::Help => {
            print!("{}", rusterisk_server::cli::HELP);
            Ok(())
        }
        Command::Version => {
            println!("rusterisk {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Check {
            directory,
            data_directory,
        } => {
            let config = Config::load(&directory).map_err(AppError::Config)?;
            runtime()?
                .block_on(Server::validate_with_data_directory(
                    &config,
                    &data_directory,
                ))
                .map_err(AppError::Control)?;
            println!("Configuration valid.");
            Ok(())
        }
        Command::Run {
            directory,
            data_directory,
        } => {
            let config = Config::load(&directory).map_err(AppError::Config)?;
            let subscriber = tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::INFO)
                .finish();
            tracing::subscriber::set_global_default(subscriber).map_err(AppError::Logging)?;
            runtime()?.block_on(start(config, data_directory))
        }
    }
}

fn main() -> ExitCode {
    let result = Command::parse(std::env::args_os().skip(1))
        .map_err(AppError::Arguments)
        .and_then(execute);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rusterisk: {error}");
            ExitCode::from(error.status())
        }
    }
}
