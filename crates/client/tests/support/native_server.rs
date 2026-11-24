use rusterisk_client::{api::ChannelId, Client, Error, Session};
use rusterisk_server::{media::MediaHub, signaling::Signaling, storage::StorageWorker, Controller};
use std::{future::IntoFuture, net::Ipv4Addr, path::Path, time::Duration};

fn failure(error: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(error.to_string()))
}

fn combine(primary: Result<(), Error>, cleanup: Result<(), Error>) -> Result<(), Error> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(Error::Cleanup(format!(
            "{primary}; cleanup failed: {cleanup}"
        ))),
    }
}

async fn joined<E: std::fmt::Display>(
    mut task: tokio::task::JoinHandle<Result<(), E>>,
) -> Result<(), Error> {
    match tokio::time::timeout(Duration::from_secs(3), &mut task).await {
        Ok(result) => result.map_err(failure)?.map_err(failure),
        Err(primary) => {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Err(Error::Timeout),
                Err(error) => Err(Error::Cleanup(format!("{primary}; join failed: {error}"))),
                Ok(Err(error)) => Err(Error::Cleanup(format!("{primary}; worker failed: {error}"))),
                Ok(Ok(())) => Err(Error::Timeout),
            }
        }
    }
}

pub async fn run(
    root: &Path,
    application: &str,
    compare: impl for<'a> AsyncFnOnce(
        &'a Client,
        &'a mut Session,
        &'a str,
        Ipv4Addr,
        &'a str,
    ) -> Result<(), Error>,
) -> Result<(), Error> {
    let configuration = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (controller, runner, _effects) = Controller::new(configuration.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(
        configuration,
        controller.clone(),
        runner,
        media.clone(),
        notices,
    )
    .await
    .map_err(failure)?;
    let (storage, worker) = StorageWorker::start(root.to_owned())
        .await
        .map_err(failure)?;
    let signaling = match signaling.try_with_control(|runner| runner.with_storage(storage.clone()))
    {
        Ok(signaling) => signaling.with_storage(storage),
        Err(primary) => {
            return combine(Err(failure(primary)), worker.close().await.map_err(failure));
        }
    };
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let server = tokio::spawn(
        axum::serve(
            listener,
            rusterisk_server::http::router_with_media(controller.clone(), media),
        )
        .into_future(),
    );
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
        let application = ChannelId::try_from(application.to_owned())?;
        let mut session = client.subscribe(&application).await?;
        let result = compare(
            &client,
            &mut session,
            application.as_str(),
            Ipv4Addr::LOCALHOST,
            &format!("http://{address}"),
        )
        .await;
        combine(result, session.shutdown().await)
    })
    .await
    .map_err(|_| Error::Timeout)
    .and_then(|result| result);
    controller.shutdown();
    let result = combine(result, joined(signaling).await);
    let result = combine(result, joined(hub).await);
    server.abort();
    let result = combine(
        result,
        match server.await {
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(failure(error)),
            Ok(result) => result.map_err(failure),
        },
    );
    combine(result, worker.close().await.map_err(failure))
}
