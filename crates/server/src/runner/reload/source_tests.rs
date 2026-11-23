use super::{load_startup_configuration, DirectoryReloadRequest};
use crate::{ControlError, Controller, Credentials, Runner};
use rusterisk_config::{Config, ConfigError};
use rusterisk_core::Wake;
use std::{future::Future, path::Path, sync::mpsc, time::Duration};
use tokio::{sync::oneshot, time::Instant};

const STARTING_ORIGIN: &str = "https://running.example";
const REPLACEMENT_ORIGIN: &str = "https://replacement.example";

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn require(condition: bool, message: &'static str) -> Result<(), ControlError> {
    condition
        .then_some(())
        .ok_or(ControlError::Invalid(message))
}

fn combine<T>(
    primary: Result<T, ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<T, ControlError> {
    match (primary, cleanup) {
        (primary, Ok(())) => primary,
        (Ok(_), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(failure(format!(
            "{primary}; configuration loader fixture cleanup failed: {cleanup}"
        ))),
    }
}

async fn bounded<T>(
    future: impl Future<Output = Result<T, ControlError>>,
) -> Result<T, ControlError> {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .map_err(failure)?
}

fn users(password: &str, origin: &str) -> String {
    format!("[general]\nallowed_origins={origin}\n[writer]\ntype=user\npassword={password}\n[reader]\ntype=user\npassword=reader-password\nread_only=yes\n")
}

fn configuration(password: &str, origin: &str) -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        &users(password, origin),
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n",
        "",
        "",
    )
    .map_err(failure)
}

struct Inputs {
    directory: tempfile::TempDir,
}

impl Inputs {
    fn new() -> Result<Self, ControlError> {
        let directory = tempfile::tempdir().map_err(failure)?;
        let prepared = (|| {
            for (file, text) in [
                ("http.conf", "[general]\nbindaddr=127.0.0.1\n".to_owned()),
                ("ari.conf", users("starting-password", STARTING_ORIGIN)),
                (
                    "pjsip.conf",
                    "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n".to_owned(),
                ),
                ("rtp.conf", String::new()),
                ("extensions.conf", String::new()),
                ("chan_websocket.conf", String::new()),
                ("websocket_client.conf", String::new()),
                ("musiconhold.conf", String::new()),
            ] {
                std::fs::write(directory.path().join(file), text).map_err(failure)?;
            }
            Ok::<_, ControlError>(())
        })();
        match prepared {
            Ok(()) => Ok(Self { directory }),
            Err(primary) => combine(Err(primary), directory.close().map_err(failure)),
        }
    }

    fn close(self, primary: Result<(), ControlError>) -> Result<(), ControlError> {
        combine(primary, self.directory.close().map_err(failure))
    }
}

fn checkpoint_failure(error: impl std::fmt::Display) -> ConfigError {
    ConfigError {
        file: "<loader fixture>".into(),
        line: 0,
        reason: format!("Owned loader checkpoint failed: {error}"),
    }
}

fn gated_load(
    entered: oneshot::Sender<()>,
    release: mpsc::Receiver<()>,
) -> impl FnOnce(&Path) -> Result<Config, ConfigError> + Send + 'static {
    move |directory| {
        entered.send(()).map_err(|()| {
            checkpoint_failure("fixture stopped before observing the started loader")
        })?;
        release
            .recv_timeout(Duration::from_secs(5))
            .map_err(checkpoint_failure)?;
        load_startup_configuration(directory)
    }
}

async fn entered(receiver: oneshot::Receiver<()>) -> Result<(), ControlError> {
    bounded(async { receiver.await.map_err(failure) }).await
}

async fn reap(runner: &mut Runner) -> Result<(), ControlError> {
    let mut result = Ok(());
    while let Some(completed) = runner.reload.loaders.join_next().await {
        result = combine(result, runner.loading_completed(completed));
    }
    result
}

#[tokio::test]
async fn a_cancelled_started_loader_keeps_admission_busy_until_it_is_reaped(
) -> Result<(), ControlError> {
    let inputs = Inputs::new()?;
    let result = async {
        let config = configuration("starting-password", STARTING_ORIGIN)?;
        let (_controller, mut runner, _effects) = Controller::new(config, 1);
        let (reply, caller) = oneshot::channel();
        let (started, observed) = oneshot::channel();
        let (release, waiting) = mpsc::sync_channel(1);
        runner.reload.begin_loading(
            inputs.directory.path().to_owned(),
            reply,
            gated_load(started, waiting),
        );
        let primary = async {
            entered(observed).await?;
            drop(caller);
            runner.tick_reload()?;
            let (reply, rejected) = oneshot::channel();
            runner.reload.begin_loading(
                inputs.directory.path().to_owned(),
                reply,
                load_startup_configuration,
            );
            let rejected = bounded(async { rejected.await.map_err(failure) }).await?;
            require(
                rejected == Err(ControlError::Capacity),
                "Cancellation released admission before the started loader was reaped",
            )
        }
        .await;
        let primary = combine(primary, release.send(()).map_err(failure));
        let primary = combine(primary, bounded(reap(&mut runner)).await);
        let primary = primary.and_then(|()| {
            require(
                runner.reload.loaders.is_empty() && runner.reload.pending.is_none(),
                "Reaped cancelled load retained its pending ownership",
            )
        });
        let cleanup = runner
            .close_reload(Instant::now() + Duration::from_secs(5))
            .await;
        combine(primary, cleanup)
    }
    .await;
    inputs.close(result)
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_load_reports_no_wake_while_its_loader_runs() -> Result<(), ControlError> {
    let inputs = Inputs::new()?;
    let result = async {
        let config = configuration("starting-password", STARTING_ORIGIN)?;
        let (_controller, mut runner, _effects) = Controller::new(config, 1);
        let (reply, caller) = oneshot::channel();
        let (started, observed) = oneshot::channel();
        let (release, waiting) = mpsc::sync_channel(1);
        runner.reload.begin_loading(
            inputs.directory.path().to_owned(),
            reply,
            gated_load(started, waiting),
        );
        let primary = async {
            entered(observed).await?;
            require(
                runner.wake()? == Wake::At(10_000),
                "A running load reported no preparation deadline",
            )?;
            drop(caller);
            runner.tick_reload()?;
            require(
                runner.wake()? == Wake::Idle,
                "A cancelled load reported a wake that only its running loader can end",
            )
        }
        .await;
        let primary = combine(primary, release.send(()).map_err(failure));
        let primary = combine(primary, bounded(reap(&mut runner)).await);
        let cleanup = runner
            .close_reload(Instant::now() + Duration::from_secs(5))
            .await;
        combine(primary, cleanup)
    }
    .await;
    inputs.close(result)
}

#[tokio::test]
async fn close_reaps_the_started_loader_and_preserves_the_primary_failure(
) -> Result<(), ControlError> {
    let inputs = Inputs::new()?;
    let result = async {
        let config = configuration("starting-password", STARTING_ORIGIN)?;
        let (_controller, mut runner, _effects) = Controller::new(config, 1);
        let (reply, caller) = oneshot::channel();
        let (started, observed) = oneshot::channel();
        let (release, waiting) = mpsc::sync_channel(1);
        runner.reload.begin_loading(
            inputs.directory.path().to_owned(),
            reply,
            gated_load(started, waiting),
        );
        let primary = entered(observed).await;
        let expected = ControlError::Adapter("primary-before-loader-shutdown".into());
        runner.retain_reload_failure(&expected);
        let primary = {
            let closed = runner.close_reload(Instant::now() + Duration::from_secs(5));
            tokio::pin!(closed);
            let pending = require(
                futures_util::poll!(closed.as_mut()).is_pending(),
                "Control close completed before the started loader was reaped",
            );
            let primary = combine(primary, pending);
            let primary = combine(primary, release.send(()).map_err(failure));
            combine(primary, bounded(closed).await)
        };
        let acknowledged = bounded(async { caller.await.map_err(failure) }).await;
        let preserved = acknowledged.and_then(|result| {
            require(
                result == Err(expected),
                "Loader shutdown replaced the caller's primary failure",
            )
        });
        let primary = combine(primary, preserved);
        let primary = primary.and_then(|()| {
            require(
                runner.reload.loaders.is_empty(),
                "Control close left an owned loader unreaped",
            )
        });
        let cleanup = runner
            .close_reload(Instant::now() + Duration::from_secs(5))
            .await;
        combine(primary, cleanup)
    }
    .await;
    inputs.close(result)
}

#[tokio::test]
async fn directory_requests_use_current_credentials_origin_and_write_permission(
) -> Result<(), ControlError> {
    for (user, password, origin, expected) in [
        (
            "writer",
            "starting-password",
            REPLACEMENT_ORIGIN,
            ControlError::Unauthorized,
        ),
        (
            "writer",
            "replacement-password",
            STARTING_ORIGIN,
            ControlError::Forbidden,
        ),
        (
            "reader",
            "reader-password",
            REPLACEMENT_ORIGIN,
            ControlError::Forbidden,
        ),
        (
            "writer",
            "replacement-password",
            REPLACEMENT_ORIGIN,
            ControlError::Unsupported,
        ),
    ] {
        let starting = configuration("starting-password", STARTING_ORIGIN)?;
        let (_controller, mut runner, _effects) = Controller::new(starting, 1);
        let (reply, result) = oneshot::channel();
        let queued = DirectoryReloadRequest {
            credentials: Credentials::new(user.into(), password.into())?,
            origin: Some(origin.into()),
            reply,
        };
        runner.config = configuration("replacement-password", REPLACEMENT_ORIGIN)?;
        runner.begin_directory_reload(queued);
        let result = bounded(async { result.await.map_err(failure) }).await?;
        require(
            result == Err(expected),
            "Directory reload ignored current authorization or unavailable-source semantics",
        )?;
        require(
            runner.reload.pending.is_none() && runner.reload.loaders.is_empty(),
            "Rejected directory request created loading or publication ownership",
        )?;
    }
    Ok(())
}
