use rusterisk_config::{Config, ConfigError};
use rusterisk_server::cli::{CliError, Command};
use rusterisk_server::runtime::Server;
use rusterisk_server::ControlError;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

#[cfg(all(test, unix))]
mod shutdown_signal_tests;

enum AppError {
    Arguments(CliError),
    Config(ConfigError),
    Control(ControlError),
    Runtime(io::Error),
    Logging(tracing::subscriber::SetGlobalDefaultError),
    Remote(rusterisk_cli::Error),
    Shutdown {
        signal: io::Error,
        cleanup: Option<ControlError>,
    },
}

impl AppError {
    const fn status(&self) -> u8 {
        match self {
            Self::Arguments(_)
            | Self::Config(_)
            | Self::Control(ControlError::Configuration(_)) => 2,
            Self::Control(_) | Self::Runtime(_) | Self::Logging(_) | Self::Shutdown { .. } => 1,
            Self::Remote(error) => error.exit_status(),
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
            Self::Remote(error) => error.fmt(f),
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

struct ShutdownSignal {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(not(unix))]
    interrupt: tokio::signal::windows::CtrlC,
}

impl ShutdownSignal {
    fn register() -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                interrupt: tokio::signal::unix::signal(
                    tokio::signal::unix::SignalKind::interrupt(),
                )?,
                terminate: tokio::signal::unix::signal(
                    tokio::signal::unix::SignalKind::terminate(),
                )?,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                interrupt: tokio::signal::windows::ctrl_c()?,
            })
        }
    }

    async fn recv(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        let received = tokio::select! {
            received = self.interrupt.recv() => received,
            received = self.terminate.recv() => received,
        };
        #[cfg(not(unix))]
        let received = self.interrupt.recv().await;
        received.ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "Shutdown signal channel closed")
        })
    }
}

async fn start(directory: PathBuf, data_directory: PathBuf) -> Result<(), AppError> {
    let mut shutdown = ShutdownSignal::register().map_err(AppError::Runtime)?;
    let server = Server::bind_from_directory(directory, 64, data_directory)
        .await
        .map_err(AppError::Control)?;
    let controller = server.controller();
    tracing::info!(http = %server.local_addr().map_err(AppError::Control)?, sip = ?server.sip_listeners().map_err(AppError::Control)?, "server ready");
    let serving = server.run();
    tokio::pin!(serving);
    tokio::select! {
        result = &mut serving => result.map_err(AppError::Control),
        signal = shutdown.recv() => {
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
        Command::Reload { server, username } => {
            let options = rusterisk_cli::RemoteOptions { server, username };
            let message = rusterisk_cli::run(rusterisk_cli::Operation::Reload, &options)
                .map_err(AppError::Remote)?;
            println!("{message}");
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
            let subscriber = tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::INFO)
                .finish();
            tracing::subscriber::set_global_default(subscriber).map_err(AppError::Logging)?;
            runtime()?.block_on(start(directory, data_directory))
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
