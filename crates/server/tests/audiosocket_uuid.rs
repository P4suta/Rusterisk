use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine;
use http_body_util::BodyExt;
use rusterisk_api::{Channel, EventKind};
use rusterisk_config::Config;
use rusterisk_server::{
    http::router, media::MediaHub, signaling::Signaling, ControlError, Controller, Credentials,
};
use rusterisk_test_support::Wait;
use std::fmt::Display;
use tokio::{io::AsyncReadExt, task::JoinHandle};
use tower::ServiceExt;

fn failure(error: impl Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn request(query: &str) -> Result<Request<Body>, ControlError> {
    Request::builder()
        .method("POST")
        .uri(format!("/ari/channels/externalMedia?{query}"))
        .header(
            "Authorization",
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("app:test-password")
            ),
        )
        .body(Body::empty())
        .map_err(failure)
}

async fn finish<T, E: Display>(mut task: JoinHandle<Result<T, E>>) -> Result<T, ControlError> {
    match Wait::Ready.bounded(&mut task).await {
        Ok(result) => result.map_err(failure)?.map_err(failure),
        Err(primary) => {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Err(failure(primary)),
                Err(error) => Err(failure(format!("{primary}; cleanup failed: {error}"))),
                Ok(Err(error)) => Err(failure(format!("{primary}; worker failed: {error}"))),
                Ok(Ok(_)) => Err(failure(primary)),
            }
        }
    }
}

#[tokio::test]
async fn http_uuid_spelling_survives_stasis_entry_and_preserves_binary_identity(
) -> Result<(), ControlError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let address = listener.local_addr().map_err(failure)?;
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let media_stop = controller.shutdown_signal();
    let signal_stop = controller.shutdown_signal();
    let hub = tokio::spawn(hub.run(media_stop));
    let signaling = tokio::spawn(signaling.run(signal_stop));
    let mut peers = Vec::with_capacity(3);
    let result = async {
        let mut subscription = controller
            .subscribe(
                Credentials::new("app".into(), "test-password".into())?,
                None,
                "calls".into(),
            )
            .await?;
        let app = router(controller.clone());
        let mut observations = Vec::with_capacity(3);
        for (index, data) in [
            "ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDEF",
            "aBcDeFaB-cDeF-4aBc-8DeF-AbCdEfAbCdEf",
            "abcdefab-cdef-4abc-8def-abcdefabcdef",
        ]
        .into_iter()
        .enumerate()
        {
            let query = format!("app=calls&external_host={address}&format=slin&channelId=external-{index}&transport=tcp&encapsulation=audiosocket&data={data}");
            let response = app
                .clone()
                .oneshot(request(&query)?)
                .await
                .map_err(failure)?;
            if response.status() != StatusCode::OK {
                return Err(failure(format!(
                    "AudioSocket creation returned {}",
                    response.status()
                )));
            }
            let channel: Channel = serde_json::from_slice(
                &response
                    .into_body()
                    .collect()
                    .await
                    .map_err(failure)?
                    .to_bytes(),
            )?;
            let (mut peer, _) =
                Wait::Ready.bounded(listener.accept())
                    .await
                    .map_err(failure)?
                    .map_err(failure)?;
            let mut identity = [0; 19];
            Wait::Ready.bounded(peer.read_exact(&mut identity))
                .await
                .map_err(failure)?
                .map_err(failure)?;
            peers.push(peer);
            let args = Wait::Ready.bounded(async {
                for _ in 0..16 {
                    let event = subscription
                        .events
                        .recv()
                        .await
                        .ok_or(ControlError::Capacity)?;
                    if let EventKind::StasisStart {
                        channel: started,
                        args,
                        ..
                    } = event.kind
                    {
                        if started.id == channel.id {
                            return Ok(args);
                        }
                    }
                }
                Err(ControlError::Capacity)
            })
            .await
            .map_err(failure)??;
            observations.push((data, args, identity));
        }
        Ok(observations)
    }
    .await;
    controller.shutdown();
    let (signaling, hub) = tokio::join!(finish(signaling), finish(hub));
    let cleanup = signaling.and(hub);
    let observations = match (result, cleanup) {
        (Ok(observations), Ok(())) => observations,
        (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
        (Err(primary), Err(cleanup)) => {
            return Err(failure(format!("{primary}; cleanup failed: {cleanup}")))
        }
    };
    drop(peers);
    for (data, args, identity) in observations {
        assert_eq!(args, [data]);
        assert_eq!(&identity[..3], &[1, 0, 16]);
        assert_eq!(
            &identity[3..],
            &[
                0xab, 0xcd, 0xef, 0xab, 0xcd, 0xef, 0x4a, 0xbc, 0x8d, 0xef, 0xab, 0xcd, 0xef, 0xab,
                0xcd, 0xef,
            ]
        );
    }
    Ok(())
}
