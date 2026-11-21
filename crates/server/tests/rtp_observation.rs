use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_core::PeerMediaDirection;
use rusterisk_media::{AudioFormat, Frame, Gate, RtpPacket, RtpSender, MAX_PACKET};
use rusterisk_server::{
    http::router_with_media,
    media::MediaHub,
    rtp::{RtpSession, SocketPair},
    Action, ControlError, Controller, Credentials, IncomingCall, IncomingTarget,
};
use serde_json::Value;
use std::{net::Ipv4Addr, time::Duration};
use tower::ServiceExt;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

async fn read(
    app: &Router,
    id: &str,
    authenticated: bool,
) -> Result<(StatusCode, Value), ControlError> {
    let mut req = Request::builder().uri(format!("/rusterisk/channels/{id}/rtp-statistics"));
    if authenticated {
        req = req.header("Authorization", "Basic cmVhZGVyOnBhc3N3b3Jk");
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::empty()).map_err(failure)?)
        .await
        .unwrap_or_else(|never| match never {});
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(failure)?
        .to_bytes();
    Ok((status, serde_json::from_slice(&body)?))
}

#[tokio::test]
async fn a_reader_observes_peer_datagrams_and_a_reused_channel_starts_without_its_predecessor(
) -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=write\n[reader]\ntype=user\npassword=password\nread_only=yes\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config, 6);
    let (media, hub, _notices) = MediaHub::new(6);
    let stopped = controller.shutdown_signal();
    let runner = tokio::spawn(runner.run());
    let hub = tokio::spawn(hub.run(stopped));
    let app = router_with_media(controller.clone(), media.clone());
    let exercise = async {
        let writer = Credentials::new("app".into(), "write".into())?;
        let _subscription = controller
            .subscribe(writer.clone(), None, "app".into())
            .await?;
        let incoming = |id: &str| {
            Ok::<_, ControlError>(IncomingCall {
                id: ChannelId::try_from(id.to_owned())?,
                endpoint: "desk".into(),
                caller: "100".into(),
                context: "internal".into(),
                extension: "200".into(),
                target: IncomingTarget::Stasis {
                    application: "app".into(),
                    arguments: Vec::new(),
                },
            })
        };
        assert_eq!(read(&app, "a", false).await?.0, StatusCode::UNAUTHORIZED);
        assert_eq!(read(&app, "a", true).await?.0, StatusCode::NOT_FOUND);
        let a = controller.incoming(incoming("a")?).await?;
        let b = controller.incoming(incoming("b")?).await?;
        let (status, unavailable) = read(&app, "a", true).await?;
        assert_eq!(status, StatusCode::OK);
        assert!(unavailable["local"].is_null());
        assert!(unavailable["peer"].is_null());
        let ip = Ipv4Addr::LOCALHOST.into();
        let alice = SocketPair::bind(ip, 0, 0).map_err(failure)?;
        let bob = SocketPair::bind(ip, 0, 0).map_err(failure)?;
        let mut destinations = Vec::new();
        for (key, peer) in [(&a, &alice), (&b, &bob)] {
            let sockets = SocketPair::bind(ip, 0, 0).map_err(failure)?;
            destinations.push(sockets.local_addr().map_err(failure)?);
            media
                .attach(
                    key.clone(),
                    sockets,
                    RtpSession {
                        remote: peer.local_addr().map_err(failure)?,
                        format: AudioFormat::Pcmu,
                        payload_type: 0,
                        telephone: None,
                    },
                    PeerMediaDirection::SendOnly,
                )
                .await
                .map_err(failure)?;
        }
        let mut packet = [0; MAX_PACKET];
        assert!(tokio::time::timeout(
            Duration::from_millis(65),
            alice.data().recv_from(&mut packet),
        )
        .await
        .is_err());
        let idle = read(&app, "a", true).await?.1;
        assert_eq!(idle["local"]["source"], "server_rtp");
        assert_eq!(idle["local"]["rxcount"], 0);
        assert_eq!(idle["local"]["txcount"], 0);
        assert_eq!(idle["generation"], a.generation);
        assert!(idle["observed_at"]
            .as_str()
            .is_some_and(|time| time.parse::<jiff::Timestamp>().is_ok()));
        let mut sender = RtpSender::new(AudioFormat::Pcmu, 0, [2; 10]).map_err(failure)?;
        for (peer, destination, count) in [(&alice, destinations[0], 2), (&bob, destinations[1], 1)]
        {
            for _ in 0..count {
                let bytes = sender
                    .packet(Frame::new(&[900; 160], 8000).map_err(failure)?)
                    .map_err(failure)?
                    .encode()
                    .map_err(failure)?;
                peer.data()
                    .send_to(&bytes, destination)
                    .await
                    .map_err(failure)?;
            }
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if read(&app, "a", true).await?.1["local"]["rxcount"] == 2
                    && read(&app, "b", true).await?.1["local"]["rxcount"] == 1
                {
                    return Ok::<_, ControlError>(());
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(failure)??;
        assert!(media
            .peer_direction(a.clone(), PeerMediaDirection::SendReceive, Gate::Open)
            .await
            .map_err(failure)?);
        let (length, sender) =
            tokio::time::timeout(Duration::from_secs(2), alice.data().recv_from(&mut packet))
                .await
                .map_err(failure)?
                .map_err(failure)?;
        assert_eq!(sender, destinations[0]);
        assert_eq!(
            RtpPacket::parse(&packet[..length])
                .map_err(failure)?
                .payload_type(),
            0
        );
        let sent = read(&app, "a", true).await?.1;
        assert!(sent["local"]["txcount"]
            .as_u64()
            .is_some_and(|count| count >= 1));
        assert_eq!(sent["local"]["rxcount"], 2);
        controller
            .execute(writer, None, Action::Hangup(a.id.clone()))
            .await?;
        let replacement = controller.incoming(incoming("a")?).await?;
        let observed = read(&app, "a", true).await?.1;
        assert_eq!(observed["generation"], replacement.generation);
        assert_ne!(replacement.generation, a.generation);
        assert!(observed["local"].is_null());
        media
            .attach(
                replacement,
                SocketPair::bind(ip, 0, 0).map_err(failure)?,
                RtpSession {
                    remote: alice.local_addr().map_err(failure)?,
                    format: AudioFormat::Pcmu,
                    payload_type: 0,
                    telephone: None,
                },
                PeerMediaDirection::SendOnly,
            )
            .await
            .map_err(failure)?;
        let (status, fresh) = read(&app, "a", true).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(fresh["local"]["rxcount"], 0);
        assert_eq!(fresh["local"]["txcount"], 0);
        assert!(fresh["peer"].is_null());
        assert_eq!(read(&app, "b", true).await?.1["local"]["rxcount"], 1);
        Ok(())
    }
    .await;
    controller.shutdown();
    let control_result = runner.await.map_err(failure)?;
    let media_result = hub.await.map_err(failure)?;
    control_result?;
    media_result.map_err(failure)?;
    exercise
}
