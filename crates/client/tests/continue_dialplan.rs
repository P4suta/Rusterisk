use rusterisk_client::{api::ChannelId, Client, Error};
use rusterisk_server::test_time::Wait;
use std::{collections::BTreeMap, future::IntoFuture};

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

#[tokio::test]
async fn continuation_preserves_omitted_positions_and_encodes_explicit_values() -> Result<(), Error>
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (sender, mut observed) = tokio::sync::mpsc::channel(3);
    let app = axum::Router::new().route(
        "/ari/channels/incoming/continue",
        axum::routing::post(
            move |axum::extract::Query(query): axum::extract::Query<BTreeMap<String, String>>| {
                let sender = sender.clone();
                async move {
                    sender
                        .try_send(query)
                        .map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE)?;
                    Ok::<_, axum::http::StatusCode>(axum::http::StatusCode::NO_CONTENT)
                }
            },
        ),
    );
    let server = tokio::spawn(axum::serve(listener, app).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "password")?;
        let id = ChannelId::try_from("incoming".to_owned())?;
        client.continue_in_dialplan(&id, None, None, None).await?;
        assert_eq!(observed.recv().await.ok_or(Error::Closed)?, BTreeMap::new());
        client
            .continue_in_dialplan(&id, Some("context /&=名前"), Some("*123#"), Some(65535))
            .await?;
        assert_eq!(
            observed.recv().await.ok_or(Error::Closed)?,
            BTreeMap::from([
                ("context".into(), "context /&=名前".into()),
                ("extension".into(), "*123#".into()),
                ("priority".into(), "65535".into())
            ])
        );
        client
            .continue_in_dialplan(&id, Some(""), Some(""), Some(0))
            .await?;
        assert_eq!(
            observed.recv().await.ok_or(Error::Closed)?,
            BTreeMap::from([
                ("context".into(), "".into()),
                ("extension".into(), "".into()),
                ("priority".into(), "0".into())
            ])
        );
        for value in ["x".repeat(129), "invalid\r\n".into()] {
            assert!(matches!(
                client
                    .continue_in_dialplan(&id, Some(&value), None, None)
                    .await,
                Err(Error::Invalid(_))
            ));
            assert!(matches!(
                client
                    .continue_in_dialplan(&id, None, Some(&value), None)
                    .await,
                Err(Error::Invalid(_))
            ));
        }
        assert!(observed.try_recv().is_err());
        Ok::<(), Error>(())
    }
    .await;
    server.abort();
    assert!(server.await.is_err_and(|error| error.is_cancelled()));
    result
}

#[tokio::test]
async fn continuation_releases_an_owned_call_and_enters_the_destination_application(
) -> Result<(), Error> {
    use rusterisk_client::{api::EventKind, CallCompletion, Cleanup, Session};
    use rusterisk_server::{Controller, IncomingCall, IncomingTarget};
    use std::time::Duration;

    async fn entered(session: &mut Session, id: &ChannelId) -> Result<(), Error> {
        for _ in 0..32 {
            if matches!(session.next_event().await?.kind, EventKind::StasisStart { channel, .. } if channel.id == *id)
            {
                return Ok(());
            }
        }
        Err(Error::Capacity)
    }

    let config = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[api]\ntype=user\npassword=password\n",
        "",
        "",
        "[internal]\nexten => 200,1,Stasis(first,one)\nsame => n,Stasis(second,two)\n",
    )
    .map_err(failure)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (controller, runner, _effects) = Controller::new(config, 4);
    let mut runner = tokio::spawn(runner.run());
    let server = tokio::spawn(
        axum::serve(listener, rusterisk_server::http::router(controller.clone())).into_future(),
    );
    let result = Wait::Ready
        .bounded(async {
            let client = Client::new(&format!("http://{address}"), "api", "password")?;
            let mut first = client
                .subscribe(&ChannelId::try_from("first".to_owned())?)
                .await?;
            let mut second = client
                .subscribe(&ChannelId::try_from("second".to_owned())?)
                .await?;
            let id = ChannelId::try_from("incoming".to_owned())?;
            controller
                .incoming(IncomingCall {
                    id: id.clone(),
                    endpoint: "phone".into(),
                    caller: "100".into(),
                    context: "internal".into(),
                    extension: "200".into(),
                    target: IncomingTarget::Dialplan,
                })
                .await
                .map_err(failure)?;
            entered(&mut first, &id).await?;
            let mut call = first.adopt(&id).await?;
            client.continue_in_dialplan(&id, None, None, None).await?;
            let completed = call.wait_completion(Duration::from_secs(1)).await?;
            if completed.progress.completion() != Some(CallCompletion::StasisExited)
                || completed.cleanup != Cleanup::Complete
            {
                return Err(Error::Invalid(
                    "continuation kept the previous call ownership",
                ));
            }
            call.close().await?;
            first.shutdown().await?;
            entered(&mut second, &id).await?;
            if client.channel(&id).await?.id != id {
                return Err(Error::Invalid(
                    "continuation removed the destination channel",
                ));
            }
            second.adopt(&id).await?.close().await?;
            second.shutdown().await?;
            if !matches!(
                client.channel(&id).await,
                Err(Error::Api { status: 404, .. })
            ) {
                return Err(Error::Invalid("destination channel cleanup failed"));
            }
            Ok::<(), Error>(())
        })
        .await
        .map_err(|_| Error::Timeout)
        .and_then(|result| result);
    controller.shutdown();
    let cleanup = match Wait::Ready.bounded(&mut runner).await {
        Ok(result) => result
            .map_err(failure)
            .and_then(|result| result.map_err(failure)),
        Err(_) => {
            runner.abort();
            match runner.await {
                Err(error) if error.is_cancelled() => Err(Error::Timeout),
                Err(error) => Err(failure(error)),
                Ok(result) => combine(Err(Error::Timeout), result.map_err(failure)),
            }
        }
    };
    server.abort();
    let cleanup = combine(
        cleanup,
        match server.await {
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(failure(error)),
            Ok(result) => result.map_err(failure),
        },
    );
    combine(result, cleanup)
}
