use super::failure;
use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    HeadersExt, Method, Response, SipMessage, Uri,
};
use rusterisk_api::{ChannelId, ChannelState, Event, EventKind};
use rusterisk_config::Codec;
use rusterisk_media::MediaError;
use rusterisk_server::{
    rtp::SocketPair,
    webrtc::{WebRtcOffer, WebRtcSession},
    Action, Controller, Credentials,
};
use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Instant,
};
use tokio::{net::UdpSocket, sync::mpsc};

fn invite(peer: SocketAddr, body: &str, sequence: u32, authorization: &str) -> String {
    format!("INVITE sip:100@localhost SIP/2.0\r\nVia: SIP/2.0/UDP {peer};branch=z9hG4bK-browser-{sequence};rport\r\nFrom: <sip:device@localhost>;tag=device\r\nTo: <sip:100@localhost>\r\nCall-ID: browser-incoming\r\nCSeq: {sequence} INVITE\r\nContact: <sip:phone@{peer}>\r\nMax-Forwards: 70\r\n{authorization}Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", body.len())
}

async fn response(signal: &UdpSocket, target: SocketAddr) -> Result<Response, MediaError> {
    let mut bytes = [0; 16385];
    let (size, source) = signal.recv_from(&mut bytes).await.map_err(failure)?;
    if source != target {
        return Err(MediaError::InvalidRoute);
    }
    match SipMessage::try_from(&bytes[..size]).map_err(failure)? {
        SipMessage::Response(response) => Ok(response),
        SipMessage::Request(_) => Err(MediaError::InvalidPacket),
    }
}

async fn ack(
    signal: &UdpSocket,
    target: SocketAddr,
    response: &Response,
    sequence: u32,
) -> Result<(), MediaError> {
    let peer = signal.local_addr().map_err(failure)?;
    let (uri, branch) = if response.status_code.code() < 300 {
        let contact = response.contact_header().map_err(failure)?;
        let contact =
            rsipstack::sip::headers::typed::Contact::parse(contact.value()).map_err(failure)?;
        (
            contact.uri.to_string(),
            format!("z9hG4bK-browser-ack-{sequence}"),
        )
    } else {
        (
            "sip:100@localhost".into(),
            format!("z9hG4bK-browser-{sequence}"),
        )
    };
    let bytes = format!("ACK {uri} SIP/2.0\r\nVia: SIP/2.0/UDP {peer};branch={branch};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: browser-incoming\r\nCSeq: {sequence} ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", response.from_header().map_err(failure)?.value(), response.to_header().map_err(failure)?.value());
    signal
        .send_to(bytes.as_bytes(), target)
        .await
        .map_err(failure)?;
    Ok(())
}

pub(super) async fn answer_device(
    signal: &UdpSocket,
    target: SocketAddr,
    controller: &Controller,
    credentials: &Credentials,
    events: &mut mpsc::Receiver<Event>,
) -> Result<(ChannelId, WebRtcSession), MediaError> {
    let sockets = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let local = sockets.local_addr().map_err(failure)?;
    let offer = WebRtcOffer::new(sockets, local, &[Codec::Pcmu], Instant::now())?;
    let peer = signal.local_addr().map_err(failure)?;
    signal
        .send_to(invite(peer, offer.description(), 1, "").as_bytes(), target)
        .await
        .map_err(failure)?;
    let challenged = response(signal, target).await?;
    assert_eq!(challenged.status_code.code(), 401);
    ack(signal, target, &challenged, 1).await?;
    let challenge = WwwAuthenticate::parse(
        challenged
            .www_authenticate_header()
            .ok_or(MediaError::InvalidPacket)?
            .value(),
    )
    .map_err(failure)?;
    let uri = Uri::try_from("sip:100@localhost").map_err(failure)?;
    let qop = AuthQop::Auth {
        cnonce: "browser-client".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username: "phone",
        password: "test-password",
        nonce: &challenge.nonce,
        uri: &uri,
        realm: &challenge.realm,
        method: &Method::Invite,
        qop: Some(&qop),
        algorithm: Algorithm::Md5,
    }
    .compute();
    let authorization = format!("Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"sip:100@localhost\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"browser-client\", nc=00000001\r\n", challenge.realm, challenge.nonce);
    signal
        .send_to(
            invite(peer, offer.description(), 2, &authorization).as_bytes(),
            target,
        )
        .await
        .map_err(failure)?;
    let id = loop {
        if let EventKind::StasisStart { channel, .. } =
            events.recv().await.ok_or(MediaError::UnknownStream)?.kind
        {
            assert_eq!(channel.state, ChannelState::Ring);
            break channel.id;
        }
    };
    controller
        .execute(credentials.clone(), None, Action::Answer(id.clone()))
        .await
        .map_err(failure)?;
    let answered = loop {
        let response = response(signal, target).await?;
        if response.status_code.code() >= 200 {
            break response;
        }
    };
    assert_eq!(answered.status_code.code(), 200);
    let session = offer.accept_answer(std::str::from_utf8(&answered.body).map_err(failure)?)?;
    ack(signal, target, &answered, 2).await?;
    loop {
        if let EventKind::ChannelStateChange { channel } =
            events.recv().await.ok_or(MediaError::UnknownStream)?.kind
        {
            if channel.id == id && channel.state == ChannelState::Up {
                break;
            }
        }
    }
    Ok((id, session))
}
