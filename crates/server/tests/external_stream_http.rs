use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine;
use http_body_util::BodyExt;
use rusterisk_api::Channel;
use rusterisk_config::Config;
use rusterisk_server::{
    http::router, media::MediaHub, signaling::Signaling, ControlError, Controller, Credentials,
};
use std::time::Duration;
use tokio::io::AsyncReadExt;
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
    let result = async {
        let _subscription = controller.subscribe(Credentials::new("app".into(), "test-password".into())?, None, "calls".into()).await?;
        let app = router(controller.clone());
        let query = format!("app=calls&external_host={}&format=slin&channelId=external&transport=tcp&encapsulation=audiosocket&data=07070707-0707-0707-0707-070707070707", listener.local_addr().map_err(failure)?);
        let response = app.clone().oneshot(request(&query)?).await.map_err(failure)?;
        assert_eq!(response.status(), StatusCode::OK);
        let channel: Channel = serde_json::from_slice(&response.into_body().collect().await.map_err(failure)?.to_bytes())?;
        assert_eq!(channel.id.as_str(), "external");
        assert!(channel.name.starts_with("AudioSocket/"));
        assert!(channel.channelvars.is_none());
        let (mut peer, _) = tokio::time::timeout(Duration::from_secs(1), listener.accept()).await.map_err(failure)?.map_err(failure)?;
        let mut identity = [0; 19];
        tokio::time::timeout(Duration::from_secs(1), peer.read_exact(&mut identity)).await.map_err(failure)?.map_err(failure)?;
        assert_eq!(&identity[..3], &[1, 0, 16]);
        assert_eq!(&identity[3..], &[7; 16]);
        let response = app.oneshot(request("app=calls&external_host=127.0.0.1:12000&format=slin&transport=tcp&encapsulation=audiosocket")?).await.map_err(failure)?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: rusterisk_api::ApiError = serde_json::from_slice(&response.into_body().collect().await.map_err(failure)?.to_bytes())?;
        assert_eq!(error.message, "data can not be empty");
        Ok(())
    }.await;
    controller.shutdown();
    runner.await.map_err(failure)??;
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    result
}
