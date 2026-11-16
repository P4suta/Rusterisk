use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine;
use http_body_util::BodyExt;
use rusterisk_api::{ApiError, Channel};
use rusterisk_config::Config;
use rusterisk_media::AudioSocket;
use rusterisk_server::{
    http::router, media::MediaHub, signaling::Signaling, ControlError, Controller, Credentials,
};
use std::{fmt::Display, time::Duration};
use tokio::{io::AsyncReadExt, net::TcpStream, task::JoinHandle};
use tower::ServiceExt;

fn failure(error: impl std::fmt::Display) -> ControlError {
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

fn combine(
    primary: Result<(), ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<(), ControlError> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(failure(format!("{primary}; {cleanup}"))),
    }
}

async fn finish<E: Display>(
    role: &str,
    mut task: JoinHandle<Result<(), E>>,
) -> Result<(), ControlError> {
    match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
        Ok(result) => result
            .map_err(|error| failure(format!("{role} join: {error}")))?
            .map_err(|error| failure(format!("{role}: {error}"))),
        Err(timeout) => {
            task.abort();
            let primary = Err(failure(format!("{role} shutdown: {timeout}")));
            let cleanup = match task.await {
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(failure(format!("{role} abort: {error}"))),
                Ok(result) => result.map_err(|error| failure(format!("{role}: {error}"))),
            };
            combine(primary, cleanup)
        }
    }
}

async fn termination(peer: Option<&mut TcpStream>) -> Result<(), ControlError> {
    let Some(peer) = peer else {
        return Ok(());
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut bytes = [0; 1923];
        for _ in 0..16 {
            peer.read_exact(&mut bytes[..3]).await.map_err(failure)?;
            let length = usize::from(u16::from_be_bytes([bytes[1], bytes[2]]));
            if length > 1920 {
                return Err(ControlError::Capacity);
            }
            peer.read_exact(&mut bytes[3..length + 3])
                .await
                .map_err(failure)?;
            match AudioSocket::parse(&bytes[..length + 3]).map_err(failure)? {
                AudioSocket::Terminate => {
                    if peer.read(&mut bytes[..1]).await.map_err(failure)? != 0 {
                        return Err(ControlError::Invalid(
                            "AudioSocket sent bytes after termination",
                        ));
                    }
                    return Ok(());
                }
                AudioSocket::Audio(_) | AudioSocket::Dtmf(_) => {}
                AudioSocket::Uuid(_) => {
                    return Err(ControlError::Invalid(
                        "AudioSocket repeated its identity during shutdown",
                    ))
                }
            }
        }
        Err(ControlError::Capacity)
    })
    .await
    .map_err(|error| failure(format!("AudioSocket termination: {error}")))?
}

struct Observations {
    admitted: StatusCode,
    channel: Channel,
    identity: [u8; 19],
    rejected: StatusCode,
    error: ApiError,
}

#[tokio::test]
async fn stream_http_requests_admit_the_transport_and_keep_reference_validation_errors(
) -> Result<(), ControlError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices).await?;
    let media_stop = controller.shutdown_signal();
    let signal_stop = controller.shutdown_signal();
    let runner = tokio::spawn(runner.run());
    let hub = tokio::spawn(hub.run(media_stop));
    let signaling = tokio::spawn(signaling.run(signal_stop));
    let mut peer = None;
    let result = async {
        let _subscription = controller.subscribe(Credentials::new("app".into(), "test-password".into())?, None, "calls".into()).await?;
        let app = router(controller.clone());
        let query = format!("app=calls&external_host={}&format=slin&channelId=external&transport=tcp&encapsulation=audiosocket&data=07070707-0707-0707-0707-070707070707", listener.local_addr().map_err(failure)?);
        let response = app.clone().oneshot(request(&query)?).await.map_err(failure)?;
        let admitted = response.status();
        if admitted != StatusCode::OK {
            return Err(failure(format!("AudioSocket admission returned {admitted}")));
        }
        let channel: Channel = serde_json::from_slice(&response.into_body().collect().await.map_err(failure)?.to_bytes())?;
        let (accepted, _) = tokio::time::timeout(Duration::from_secs(1), listener.accept()).await.map_err(failure)?.map_err(failure)?;
        peer = Some(accepted);
        let current = peer.as_mut().ok_or(ControlError::Capacity)?;
        let mut identity = [0; 19];
        tokio::time::timeout(Duration::from_secs(1), current.read_exact(&mut identity)).await.map_err(failure)?.map_err(failure)?;
        let response = app.oneshot(request("app=calls&external_host=127.0.0.1:12000&format=slin&transport=tcp&encapsulation=audiosocket")?).await.map_err(failure)?;
        let rejected = response.status();
        if rejected != StatusCode::BAD_REQUEST {
            return Err(failure(format!("AudioSocket validation returned {rejected}")));
        }
        let error = serde_json::from_slice(&response.into_body().collect().await.map_err(failure)?.to_bytes())?;
        Ok(Observations { admitted, channel, identity, rejected, error })
    }.await;
    controller.shutdown();
    let (runner, signaling, hub, drained) = tokio::join!(
        finish("runner", runner),
        finish("signaling", signaling),
        finish("media", hub),
        termination(peer.as_mut()),
    );
    let cleanup = combine(combine(combine(runner, signaling), hub), drained);
    let observed = match (result, cleanup) {
        (Ok(observed), Ok(())) => observed,
        (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
        (Err(primary), Err(cleanup)) => {
            return Err(failure(format!("{primary}; cleanup failed: {cleanup}")))
        }
    };
    drop(peer);
    assert_eq!(observed.admitted, StatusCode::OK);
    assert_eq!(observed.channel.id.as_str(), "external");
    assert!(observed.channel.name.starts_with("AudioSocket/"));
    assert!(observed.channel.channelvars.is_none());
    assert_eq!(&observed.identity[..3], &[1, 0, 16]);
    assert_eq!(&observed.identity[3..], &[7; 16]);
    assert_eq!(observed.rejected, StatusCode::BAD_REQUEST);
    assert_eq!(observed.error.message, "data can not be empty");
    Ok(())
}
