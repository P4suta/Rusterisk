use rusterisk_api::{ChannelId, EventKind, MediaControlFormat, MediaDirection, MediaFormat};
use rusterisk_config::Config;
use rusterisk_server::{
    media::MediaHub, signaling::Signaling, Action, ControlError, Controller, Credentials,
};
use rusterisk_test_support::Wait;
use std::{
    future::{poll_fn, Future},
    task::Poll,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn credentials() -> Result<Credentials, ControlError> {
    Credentials::new("app".into(), "test-password".into())
}

async fn headers(peer: &mut tokio::net::TcpStream) -> Result<(), ControlError> {
    let mut bytes = Vec::with_capacity(4096);
    loop {
        let mut byte = [0];
        peer.read_exact(&mut byte).await.map_err(failure)?;
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            return Ok(());
        }
        if bytes.len() >= 4096 {
            return Err(ControlError::Capacity);
        }
    }
}

async fn cancelled_during(retry: bool) -> Result<(), ControlError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let address = listener.local_addr().map_err(failure)?;
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n", "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[phone]\ntype=aor\nmax_contacts=1\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=phone\nallow=ulaw\n", "", "").map_err(failure)?
        .with_websocket_configuration("", &format!("[agent]\ntype=websocket_client\nconnection_type=per_call_config\nuri=ws://{address}/media?token=private-query\nprotocols=media\nconnection_timeout=1000\nreconnect_interval=1000\nreconnect_attempts=2\n")).map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let signaling_address = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let result = Wait::Scenario.bounded(async {
        let mut subscription = controller.subscribe(credentials()?, None, "calls".into()).await?;
        let id = ChannelId::try_from("cancelled".to_owned())?;
        let mut peer = {
            let mut request = std::pin::pin!(controller.execute(credentials()?, None, Action::ExternalWebSocketClient { id: id.clone(), app: "calls".into(), client: "agent".into(), format: MediaFormat::Pcmu, control_format: Some(MediaControlFormat::Json), direction: MediaDirection::Both, auto_answer: true, data: String::new() }));
            assert!(poll_fn(|context| Poll::Ready(request.as_mut().poll(context).is_pending())).await);
            let (mut peer, _) = listener.accept().await.map_err(failure)?;
            headers(&mut peer).await?;
            if retry { peer.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.map_err(failure)?; }
            let phone = tokio::net::UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
            let local = phone.local_addr().map_err(failure)?;
            let register = format!("REGISTER sip:{signaling_address} SIP/2.0\r\nVia: SIP/2.0/UDP {local};branch=z9hG4bK-pending-websocket;rport\r\nFrom: <sip:phone@localhost>;tag=phone\r\nTo: <sip:phone@localhost>\r\nCall-ID: pending-websocket\r\nCSeq: 1 REGISTER\r\nContact: <sip:phone@{local}>\r\nExpires: 20\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n");
            phone.send_to(register.as_bytes(), signaling_address).await.map_err(failure)?;
            let mut bytes = [0; 16385];
            let (size, _) = Wait::Ready.bounded(phone.recv_from(&mut bytes)).await.map_err(failure)?.map_err(failure)?;
            assert!(bytes[..size].starts_with(b"SIP/2.0 401 "));
            assert!(poll_fn(|context| Poll::Ready(request.as_mut().poll(context).is_pending())).await);
            peer
        };
        loop {
            let event = subscription.events.recv().await.ok_or(ControlError::Capacity)?;
            if matches!(event.kind, EventKind::ChannelDestroyed { channel, .. } if channel.id == id) {
                break;
            }
        }
        let mut byte = [0];
        assert_eq!(Wait::Ready.bounded(peer.read(&mut byte)).await.map_err(failure)?.map_err(failure)?, 0);
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_millis(1200)).await;
        tokio::time::resume();
        assert!(Wait::Quiet.bounded(listener.accept()).await.is_err());
        assert!(matches!(controller.execute(credentials()?, None, Action::GetChannel(id)).await, Err(ControlError::NotFound(_))));
        Ok::<(), ControlError>(())
    }).await.map_err(failure);
    controller.shutdown();
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    result?
}

#[tokio::test]
async fn cancelling_an_unanswered_upgrade_preserves_signaling_progress_and_closes_the_socket(
) -> Result<(), ControlError> {
    cancelled_during(false).await
}

#[tokio::test]
async fn cancellation_during_the_retry_delay_prevents_the_next_connection(
) -> Result<(), ControlError> {
    cancelled_during(true).await
}
