use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine;
use http_body_util::BodyExt;
use rusterisk_api::{Channel, EventKind};
use rusterisk_config::Config;
use rusterisk_media::{AudioFormat, Frame, RtpSender};
use rusterisk_server::{
    http::router, media::MediaHub, rtp::SocketPair, signaling::Signaling, ControlError, Controller,
    Credentials,
};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};
use tower::ServiceExt;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn request(method: &str, path: &str, user: &str) -> Result<Request<Body>, ControlError> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(
            "Authorization",
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:test-password"))
            ),
        )
        .body(Body::empty())
        .map_err(failure)
}

#[tokio::test]
async fn external_http_creation_returns_the_bound_transport_and_receives_audio(
) -> Result<(), ControlError> {
    let peer = SocketPair::bind(IpAddr::V4(Ipv4Addr::LOCALHOST), 0, 0).map_err(failure)?;
    let configuration = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n[reader]\ntype=user\npassword=test-password\nread_only=yes\n", "", "", "").map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(configuration.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling =
        Signaling::bind(configuration, controller.clone(), runner, media, notices).await?;
    let signal_stop = controller.shutdown_signal();
    let media_stop = controller.shutdown_signal();
    let hub = tokio::spawn(hub.run(media_stop));
    let signaling = tokio::spawn(signaling.run(signal_stop));
    let result: Result<SocketAddr, ControlError> = async {
        let mut subscription = controller.subscribe(Credentials::new("app".into(), "test-password".into())?, None, "calls".into()).await?;
        let app = router(controller.clone());
        let path = format!("/ari/channels/externalMedia?app=calls&external_host={}&format=alaw&channelId=external&data=media", peer.local_addr().map_err(failure)?);
        assert_eq!(app.clone().oneshot(request("POST", &path, "reader")?).await.map_err(failure)?.status(), StatusCode::FORBIDDEN);
        let response = app.clone().oneshot(request("POST", &path, "app")?).await.map_err(failure)?;
        assert_eq!(response.status(), StatusCode::OK);
        let channel: Channel = serde_json::from_slice(&response.into_body().collect().await.map_err(failure)?.to_bytes())?;
        let variables = channel.channelvars.ok_or(ControlError::Capacity)?;
        let address = variables.get("UNICASTRTP_LOCAL_ADDRESS").ok_or(ControlError::Capacity)?.parse::<IpAddr>().map_err(failure)?;
        let port = variables.get("UNICASTRTP_LOCAL_PORT").ok_or(ControlError::Capacity)?.parse::<u16>().map_err(failure)?;
        let local = SocketAddr::new(address, port);
        tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..16 {
                let event = subscription.events.recv().await.ok_or(ControlError::Capacity)?;
                if matches!(event.kind, EventKind::StasisStart { channel, args } if channel.id.as_str() == "external" && args == ["media"]) { return Ok(()); }
            }
            Err(ControlError::Capacity)
        }).await.map_err(failure)??;
        let mut sender = RtpSender::new(AudioFormat::Pcma, 8, [21; 10]).map_err(failure)?;
        let packet = sender.packet(Frame::new(&[4000; 160], 8000).map_err(failure)?).map_err(failure)?.encode().map_err(failure)?;
        peer.data().send_to(&packet, local).await.map_err(failure)?;
        tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..16 {
                let event = subscription.events.recv().await.ok_or(ControlError::Capacity)?;
                if matches!(event.kind, EventKind::ChannelVarset { variable, value, .. } if variable == "RUSTERISK_MEDIA_CONNECTED" && value == "1") { return Ok(()); }
            }
            Err(ControlError::Capacity)
        }).await.map_err(failure)??;
        assert_eq!(app.clone().oneshot(request("DELETE", "/ari/channels/external", "app")?).await.map_err(failure)?.status(), StatusCode::NO_CONTENT);
        for (query, status) in [
            ("", StatusCode::BAD_REQUEST),
            ("app=calls&external_host=127.0.0.1:12000", StatusCode::BAD_REQUEST),
            ("app=calls&external_host=127.0.0.1:12000&format=ulaw&transport=tcp", StatusCode::BAD_REQUEST),
            ("app=calls&external_host=127.0.0.1:12000&format=ulaw&direction=invalid", StatusCode::BAD_REQUEST),
            ("app=calls&external_host=0.0.0.0:12000&format=ulaw", StatusCode::BAD_REQUEST),
            ("app=calls&external_host=127.0.0.1:12000&format=opus", StatusCode::NOT_IMPLEMENTED),
        ] {
            assert_eq!(app.clone().oneshot(request("POST", &format!("/ari/channels/externalMedia?{query}"), "app")?).await.map_err(failure)?.status(), status, "{query}");
        }
        Ok(local)
    }.await;
    controller.shutdown();
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    let local = result?;
    let replacement =
        SocketPair::bind(local.ip(), local.port(), local.port() + 1).map_err(failure)?;
    assert_eq!(replacement.local_addr().map_err(failure)?, local);
    Ok(())
}
