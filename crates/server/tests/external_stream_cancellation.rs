use rusterisk_api::{AudioSocketTarget, ChannelId};
use rusterisk_config::Config;
use rusterisk_server::{
    media::MediaHub, signaling::Signaling, Action, ControlError, Controller, Credentials,
};
use rusterisk_test_support::Wait;
use std::{
    future::{poll_fn, Future},
    task::Poll,
};
use tokio::{io::AsyncReadExt, sync::watch};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn credentials() -> Result<Credentials, ControlError> {
    Credentials::new("app".into(), "test-password".into())
}

#[tokio::test]
async fn pending_stream_admission_keeps_registration_progress_and_cancellation_closes_the_socket(
) -> Result<(), ControlError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n", "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[phone]\ntype=aor\nmax_contacts=1\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=phone\nallow=ulaw\n", "", "").map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let signaling_address = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let signal_stop = controller.shutdown_signal();
    let media_stop = controller.shutdown_signal();
    let (resume, mut resumed) = watch::channel(false);
    let hub = tokio::spawn(async move {
        while !*resumed.borrow() {
            resumed.changed().await.map_err(failure)?;
        }
        hub.run(media_stop).await.map_err(failure)
    });
    let signaling = tokio::spawn(signaling.run(signal_stop));
    let result = async {
        let _subscription = controller.subscribe(credentials()?, None, "calls".into()).await?;
        let id = ChannelId::try_from("cancelled".to_owned())?;
        let mut peer = {
            let target = AudioSocketTarget::new(listener.local_addr().map_err(failure)?, [7; 16]).map_err(failure)?;
            let mut request = std::pin::pin!(controller.execute(credentials()?, None, Action::ExternalAudioSocket { id: id.clone(), app: "calls".into(), target }));
            assert!(poll_fn(|context| Poll::Ready(request.as_mut().poll(context).is_pending())).await);
            let (mut peer, _) = Wait::Ready.bounded(listener.accept()).await.map_err(failure)?.map_err(failure)?;
            let mut identity = [0; 19];
            Wait::Ready.bounded(peer.read_exact(&mut identity)).await.map_err(failure)?.map_err(failure)?;
            let phone = tokio::net::UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
            let local = phone.local_addr().map_err(failure)?;
            let register = format!("REGISTER sip:{signaling_address} SIP/2.0\r\nVia: SIP/2.0/UDP {local};branch=z9hG4bK-pending-stream;rport\r\nFrom: <sip:phone@localhost>;tag=phone\r\nTo: <sip:phone@localhost>\r\nCall-ID: pending-stream\r\nCSeq: 1 REGISTER\r\nContact: <sip:phone@{local}>\r\nExpires: 20\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n");
            phone.send_to(register.as_bytes(), signaling_address).await.map_err(failure)?;
            let mut bytes = [0; 16385];
            let (size, _) = Wait::Ready.bounded(phone.recv_from(&mut bytes)).await.map_err(failure)?.map_err(failure)?;
            assert!(bytes[..size].starts_with(b"SIP/2.0 401 "));
            assert!(poll_fn(|context| Poll::Ready(request.as_mut().poll(context).is_pending())).await);
            peer
        };
        Wait::Ready.bounded(async {
            loop {
                match controller.execute(credentials()?, None, Action::GetChannel(id.clone())).await {
                    Err(ControlError::NotFound(_)) => return Ok::<(), ControlError>(()),
                    Ok(_) => {},
                    Err(error) => return Err(error),
                }
            }
        }).await.map_err(failure)??;
        resume.send_replace(true);
        let mut byte = [0];
        assert_eq!(Wait::Ready.bounded(peer.read(&mut byte)).await.map_err(failure)?.map_err(failure)?, 0);
        assert!(matches!(controller.execute(credentials()?, None, Action::GetChannel(id)).await, Err(ControlError::NotFound(_))));
        Ok(())
    }.await;
    resume.send_replace(true);
    controller.shutdown();
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)??;
    result
}
