use super::failure;
use crate::media::MediaHub;
use crate::signaling::Signaling;
use crate::{Action, ControlError, Controller, Credentials};
use rsipstack::sip::{HeadersExt, Request, SipMessage};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use std::{net::SocketAddr, time::Duration};
use tokio::net::UdpSocket;

async fn received(peer: &UdpSocket) -> Result<(Request, SocketAddr), ControlError> {
    let mut bytes = [0; rusterisk_sip::MAX_MESSAGE + 1];
    let (size, address) = tokio::time::timeout(Duration::from_secs(3), peer.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    let message = SipMessage::try_from(&bytes[..size]).map_err(failure)?;
    match message {
        SipMessage::Request(request) => Ok((request, address)),
        SipMessage::Response(_) => Err(ControlError::Invalid("Expected outgoing request")),
    }
}

#[tokio::test]
async fn actual_udp_origination_retries_with_the_shared_integrity_challenge(
) -> Result<(), ControlError> {
    let peer = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let target = peer.local_addr().map_err(failure)?;
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test\n", &format!("[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[incoming]\ntype=auth\nusername=incoming\npassword=sample\n[remote]\ntype=auth\nusername=remote-user\npassword=remote-password\nrealm=provider.example\n[phone]\ntype=aor\ncontact=sip:phone@{target}\n[phone]\ntype=endpoint\ntransport=udp\nauth=incoming\noutbound_auth=remote\naors=phone\nallow=ulaw\n"), "", "").map_err(failure)?;
    let (controller, runner, effects) = Controller::new(config.clone(), 8);
    let (media, hub, notices) = MediaHub::new(8);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices).await?;
    let local = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let signal_stop = controller.shutdown_signal();
    let media_stop = controller.shutdown_signal();
    let runner = tokio::spawn(runner.run());
    let hub = tokio::spawn(hub.run(media_stop));
    let signaling = tokio::spawn(signaling.run(signal_stop));
    let result: Result<(), ControlError> = async {
        let credentials = || Credentials::new("app".into(), "test".into());
        let subscription = controller.subscribe(credentials()?, None, "app".into()).await?;
        let id = ChannelId::try_from("integrity-origination".to_owned())?;
        controller.execute(credentials()?, None, Action::Originate { id: id.clone(), endpoint: "PJSIP/phone".into(), app: "app".into(), args: Vec::new(), caller: "displayed-caller".into(), timeout: 30 }).await?;
        let (initial, address) = received(&peer).await?;
        assert_eq!(initial.method.to_string(), "INVITE");
        let challenge = format!("SIP/2.0 401 Unauthorized\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=provider\r\nCall-ID: {}\r\nCSeq: {}\r\nWWW-Authenticate: Digest realm=\"provider.example\", nonce=\"provider-nonce\", algorithm=SHA-512-256, qop=\"auth,auth-int\"\r\nContent-Length: 0\r\n\r\n", initial.top_via_header().map_err(failure)?.value(), initial.from_header().map_err(failure)?.value(), initial.to_header().map_err(failure)?.value(), initial.call_id_header().map_err(failure)?.value(), initial.cseq_header().map_err(failure)?.value());
        assert_eq!(peer.send_to(challenge.as_bytes(), address).await.map_err(failure)?, challenge.len());
        let (ack, ack_address) = received(&peer).await?;
        assert_eq!(ack.method.to_string(), "ACK");
        assert_eq!(ack_address, address);
        let (retried, retried_address) = received(&peer).await?;
        assert_eq!(retried.method.to_string(), "INVITE");
        assert_eq!(retried_address, address);
        assert_eq!(retried.body, initial.body);
        assert_eq!(retried.from_header().map_err(failure)?.value(), initial.from_header().map_err(failure)?.value());
        assert_eq!(retried.cseq_header().map_err(failure)?.value(), "2 INVITE");
        let authorization = retried.authorization_header().ok_or(ControlError::Invalid("Missing authorization"))?.value();
        assert!(authorization.contains("algorithm=SHA-512-256"));
        assert!(authorization.contains("qop=auth-int"));
        assert!(authorization.contains("username=\"remote-user\""));
        assert!(authorization.contains("nc=00000001"));
        controller.execute(credentials()?, None, Action::Hangup(id)).await?;
        drop(subscription);
        Ok(())
    }.await;
    controller.shutdown();
    runner.await.map_err(failure)??;
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    result?;
    let _released = UdpSocket::bind(local).await.map_err(failure)?;
    Ok(())
}
