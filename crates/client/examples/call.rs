use rusterisk_client::{api::ChannelId, CallMilestone, Client, Error, Originate};
use std::{future::Future, time::Duration};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Error> {
    let client = Client::new(
        &setting("RUSTERISK_URL", Some("http://127.0.0.1:8088"))?,
        &setting("RUSTERISK_USERNAME", Some("api"))?,
        &setting("RUSTERISK_PASSWORD", None)?,
    )?;
    let application = ChannelId::try_from(setting("RUSTERISK_APP", Some("calls"))?)?;
    let options = Originate::new(setting("RUSTERISK_ENDPOINT", Some("PJSIP/alice"))?)?;
    run_call(client, application, options, async {
        tokio::signal::ctrl_c().await.map_err(Error::from)
    })
    .await
}

fn setting(name: &'static str, default: Option<&str>) -> Result<String, Error> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(std::env::VarError::NotPresent) => default.map(String::from).ok_or(Error::Invalid(
            "set RUSTERISK_PASSWORD before starting the example",
        )),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(Error::Invalid("environment values must use Unicode"))
        }
    }
}

async fn run_call(
    client: Client,
    application: ChannelId,
    options: Originate,
    stop: impl Future<Output = Result<(), Error>>,
) -> Result<(), Error> {
    let session = client.subscribe(&application).await?;
    let result = async {
        let mut call = session.originate(options).await?;
        let result = tokio::select! {
            stopped = stop => stopped,
            connected = async {
                for milestone in [CallMilestone::Accepted, CallMilestone::Stasis, CallMilestone::Answered, CallMilestone::MediaConnected] {
                    call.wait(milestone, Duration::from_secs(30)).await?;
                    println!("{} {milestone:?}", call.id());
                }
                let completed = call.wait_completion(Duration::from_secs(3600)).await?;
                println!("{} {:?}", call.id(), completed.progress.completion());
                Ok::<(), Error>(())
            } => connected,
        };
        let cleanup = call.close().await;
        if let Err(error) = &cleanup { eprintln!("{error}"); }
        result.and(cleanup)
    }.await;
    let shutdown = session.shutdown().await;
    if let Err(error) = &shutdown {
        eprintln!("{error}");
    }
    result.and(shutdown)
}

#[cfg(test)]
mod tests {
    use rusterisk_client::{Client, Error, Originate};
    use std::{future::IntoFuture, time::Duration};

    #[tokio::test]
    async fn stopping_the_example_during_call_setup_releases_the_remote_channel(
    ) -> Result<(), Error> {
        let config = rusterisk_config::Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=test-password\n",
            "",
            "",
            "",
        )
        .map_err(|_| Error::Invalid("test configuration"))?;
        let (controller, runner, mut effects) = rusterisk_server::Controller::new(config, 4);
        let runner = tokio::spawn(runner.run());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(
            axum::serve(listener, rusterisk_server::http::router(controller.clone())).into_future(),
        );
        let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let result = async {
            let demo = tokio::spawn(super::run_call(
                client.clone(),
                "app".to_owned().try_into()?,
                Originate::new("PJSIP/phone".into())?,
                async { stopped.await.map_err(|_| Error::Closed) },
            ));
            let effect = tokio::time::timeout(Duration::from_secs(2), effects.recv())
                .await
                .map_err(|_| Error::Timeout)?
                .ok_or(Error::Closed)?;
            let rusterisk_server::Effect::Start { key, .. } = effect else {
                return Err(Error::Invalid("expected start effect"));
            };
            stop.send(()).map_err(|_| Error::Closed)?;
            tokio::time::timeout(Duration::from_secs(2), demo)
                .await
                .map_err(|_| Error::Timeout)?
                .map_err(|_| Error::Closed)??;
            let effect = effects.recv().await.ok_or(Error::Closed)?;
            assert!(
                matches!(effect, rusterisk_server::Effect::Release { key: ended } if ended == key)
            );
            assert!(client.channels().await?.is_empty());
            Ok(())
        }
        .await;
        controller.shutdown();
        runner
            .await
            .map_err(|_| Error::Closed)?
            .map_err(|_| Error::Closed)?;
        server.abort();
        assert!(server.await.is_err());
        result
    }
}
