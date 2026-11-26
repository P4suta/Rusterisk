use super::{acknowledgements::AckIdentity, failure, id, transfer_peer::Peer};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method, Request, SipMessage};
use rusterisk_api::{ChannelId, ChannelState, EventKind};
use rusterisk_client::{
    CallCompletion, CallMilestone, Client, Originate, Session as ClientSession,
};
use rusterisk_config::{Codec, Config};
use rusterisk_server::{runtime::Server, ControlError};
use rusterisk_sip::Session;
use rusterisk_test_support::Wait;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::net::UdpSocket;

fn check(condition: bool, reason: &'static str) -> Result<(), ControlError> {
    if condition {
        Ok(())
    } else {
        Err(ControlError::Invalid(reason))
    }
}

pub(super) async fn receive(
    socket: &UdpSocket,
    expected: Method,
) -> Result<(Request, SocketAddr), ControlError> {
    let mut bytes = [0; 16385];
    let (size, peer) = Wait::Ready
        .bounded(socket.recv_from(&mut bytes))
        .await
        .map_err(|_| failure(format!("Timed out waiting for SIP {expected}")))?
        .map_err(failure)?;
    match SipMessage::try_from(&bytes[..size]).map_err(failure)? {
        SipMessage::Request(request) if request.method == expected => Ok((request, peer)),
        SipMessage::Request(request) => Err(failure(format!(
            "Expected SIP {expected}, received {}",
            request.method
        ))),
        SipMessage::Response(response) => Err(failure(format!(
            "Expected SIP {expected}, received response {}",
            response.status_code.code()
        ))),
    }
}

pub(super) fn reply(
    request: &Request,
    status: &str,
    to: &str,
    headers: &str,
    body: &str,
) -> Result<String, ControlError> {
    Ok(format!("SIP/2.0 {status}\r\nVia: {}\r\nFrom: {}\r\nTo: {to}\r\nCall-ID: {}\r\nCSeq: {}\r\n{headers}Content-Length: {}\r\n\r\n{body}",
        request.top_via_header().map_err(failure)?.value(),
        request.from_header().map_err(failure)?.value(),
        request.call_id_header().map_err(failure)?.value(),
        request.cseq_header().map_err(failure)?.value(),
        body.len(),
    ))
}

pub(super) fn cancellation(invite: &Request, cancel: &Request) -> Result<(), ControlError> {
    let sequence = cancel
        .cseq_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?;
    let invitation = invite
        .cseq_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?;
    check(
        cancel.uri == invite.uri
            && sequence.seq == invitation.seq
            && sequence.method == Method::Cancel
            && cancel.top_via_header().map_err(failure)?.value()
                == invite.top_via_header().map_err(failure)?.value()
            && cancel.call_id_header().map_err(failure)?.value()
                == invite.call_id_header().map_err(failure)?.value()
            && cancel.from_header().map_err(failure)?.value()
                == invite.from_header().map_err(failure)?.value()
            && cancel.to_header().map_err(failure)?.value()
                == invite.to_header().map_err(failure)?.value(),
        "CANCEL changed the original INVITE transaction",
    )
}

pub(super) fn teardown(ack: &Request, bye: &Request) -> Result<(), ControlError> {
    let accepted = ack
        .cseq_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?;
    let sequence = bye
        .cseq_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?;
    check(
        bye.uri == ack.uri
            && accepted.seq.checked_add(1) == Some(sequence.seq)
            && sequence.method == Method::Bye
            && bye.call_id_header().map_err(failure)?.value()
                == ack.call_id_header().map_err(failure)?.value()
            && bye.from_header().map_err(failure)?.value()
                == ack.from_header().map_err(failure)?.value()
            && bye.to_header().map_err(failure)?.value()
                == ack.to_header().map_err(failure)?.value(),
        "BYE changed the late answer dialog",
    )
}

async fn assert_no_connected_event(
    events: &mut ClientSession,
    channel_id: &ChannelId,
) -> Result<(), ControlError> {
    let result = Wait::Observation.bounded(async {
        for _ in 0..64 {
            let event = events.next_event().await.map_err(failure)?;
            let connected = matches!(
                event.kind,
                EventKind::StasisStart { channel, .. } | EventKind::ChannelStateChange { channel }
                    if channel.id == *channel_id && channel.state == ChannelState::Up
            );
            check(!connected, "A late answer published a connected channel")?;
        }
        Err(ControlError::Capacity)
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => Ok(()),
    }
}

async fn exchange(
    client: &Client,
    events: &mut ClientSession,
    peer: &Peer,
) -> Result<(), ControlError> {
    let mut call = events
        .originate(Originate::new("PJSIP/phone".into()).map_err(failure)?)
        .await
        .map_err(failure)?;
    call.wait(CallMilestone::Accepted, Wait::Ready.duration())
        .await
        .map_err(failure)?;
    let channel_id = call.id().clone();
    let (invite, target) = receive(&peer.signal, Method::Invite).await?;
    let offer = Session::parse(
        std::str::from_utf8(&invite.body).map_err(failure)?,
        &[Codec::Pcmu],
    )
    .map_err(failure)?;
    let tagged = format!(
        "{};tag=late-answer",
        invite.to_header().map_err(failure)?.value()
    );
    let ringing = reply(&invite, "180 Ringing", &tagged, "", "")?;
    peer.signal
        .send_to(ringing.as_bytes(), target)
        .await
        .map_err(failure)?;

    call.cancel();
    let (cancel, source) = receive(&peer.signal, Method::Cancel).await?;
    check(source == target, "CANCEL changed the signaling peer")?;
    cancellation(&invite, &cancel)?;
    let cancelled = reply(&cancel, "200 OK", &tagged, "", "")?;
    peer.signal
        .send_to(cancelled.as_bytes(), target)
        .await
        .map_err(failure)?;

    let accepted_contact = format!("sip:answered@{}", peer.contact()?);
    let headers = format!("Contact: <{accepted_contact}>\r\nContent-Type: application/sdp\r\n");
    let body = offer.answer(peer.media()?, 1).map_err(failure)?;
    let late = reply(&invite, "200 OK", &tagged, &headers, &body)?;
    let expected = AckIdentity::for_invitation(&invite, target, tagged)?;
    peer.signal
        .send_to(late.as_bytes(), target)
        .await
        .map_err(failure)?;
    let (ack, source) = receive(&peer.signal, Method::Ack).await?;
    check(
        expected.matches(&ack, source)?,
        "ACK changed the late answer identity",
    )?;
    check(
        ack.uri.to_string() == accepted_contact,
        "ACK did not use the answered Contact",
    )?;
    let (bye, source) = receive(&peer.signal, Method::Bye).await?;
    check(source == target, "BYE changed the signaling peer")?;
    teardown(&ack, &bye)?;
    let ended = reply(
        &bye,
        "200 OK",
        bye.to_header().map_err(failure)?.value(),
        "",
        "",
    )?;
    peer.signal
        .send_to(ended.as_bytes(), target)
        .await
        .map_err(failure)?;

    peer.signal
        .send_to(late.as_bytes(), target)
        .await
        .map_err(failure)?;
    let (replayed, source) = receive(&peer.signal, Method::Ack).await?;
    check(
        expected.matches(&replayed, source)? && replayed.uri == ack.uri,
        "A repeated answer changed its ACK",
    )?;
    let mut packet = [0; 16385];
    check(
        Wait::Observation
            .bounded(peer.signal.recv_from(&mut packet))
            .await
            .is_err(),
        "A repeated answer started another teardown",
    )?;
    let terminal = call
        .wait_completion(Wait::Ready.duration())
        .await
        .map_err(failure)?;
    check(
        terminal.progress.completion() == Some(CallCompletion::Cancelled),
        "The SDK lost the cancelled completion",
    )?;
    Wait::Ready
        .bounded(call.close())
        .await
        .map_err(failure)?
        .map_err(failure)?;
    check(
        client.channels().await.map_err(failure)?.is_empty(),
        "A late answer left a visible channel",
    )?;
    assert_no_connected_event(events, &channel_id).await?;

    let rtcp_port = offer
        .remote
        .port()
        .checked_add(1)
        .ok_or(ControlError::Capacity)?;
    let _rtp = UdpSocket::bind(offer.remote).await.map_err(failure)?;
    let _rtcp = UdpSocket::bind((offer.remote.ip(), rtcp_port))
        .await
        .map_err(failure)?;
    Ok(())
}

async fn ringing(peer: &Peer, invite: &Request, target: SocketAddr) -> Result<(), ControlError> {
    let tagged = format!(
        "{};tag=dial-limit",
        invite.to_header().map_err(failure)?.value()
    );
    let ringing = reply(invite, "180 Ringing", &tagged, "", "")?;
    peer.signal
        .send_to(ringing.as_bytes(), target)
        .await
        .map_err(failure)?;
    let (cancel, source) = receive(&peer.signal, Method::Cancel).await?;
    check(source == target, "CANCEL changed the signaling peer")?;
    cancellation(invite, &cancel)?;
    let cancelled = reply(&cancel, "200 OK", &tagged, "", "")?;
    peer.signal
        .send_to(cancelled.as_bytes(), target)
        .await
        .map_err(failure)?;
    let terminated = reply(invite, "487 Request Terminated", &tagged, "", "")?;
    peer.signal
        .send_to(terminated.as_bytes(), target)
        .await
        .map_err(failure)?;
    receive(&peer.signal, Method::Ack).await?;
    Ok(())
}

fn require_silent_invite(bytes: &[u8]) -> Result<(), ControlError> {
    check(
        matches!(SipMessage::try_from(bytes).map_err(failure)?, SipMessage::Request(request) if request.method == Method::Invite),
        "A silent peer received more than the repeated INVITE",
    )
}

async fn past_its_dial_limit(
    events: &mut ClientSession,
    peer: &Peer,
    answers: bool,
) -> Result<(), ControlError> {
    let mut request = Originate::new("PJSIP/phone".into()).map_err(failure)?;
    request.timeout = Duration::from_secs(1);
    let mut call = events.originate(request).await.map_err(failure)?;
    call.wait(CallMilestone::Accepted, Wait::Ready.duration())
        .await
        .map_err(failure)?;
    let channel_id = call.id().clone();
    let (invite, target) = receive(&peer.signal, Method::Invite).await?;
    if answers {
        ringing(peer, &invite, target).await?;
    }
    let mut bytes = [0; 16385];
    let ended = Wait::Ready
        .bounded(async {
            loop {
                let event = tokio::select! {
                    event = events.next_event() => event.map_err(failure)?,
                    packet = peer.signal.recv_from(&mut bytes), if !answers => {
                        let (size, _) = packet.map_err(failure)?;
                        require_silent_invite(&bytes[..size])?;
                        continue;
                    }
                };
                if let EventKind::ChannelDestroyed { cause, channel, .. } = event.kind {
                    if channel.id == channel_id {
                        return Ok::<u16, ControlError>(cause);
                    }
                }
            }
        })
        .await
        .map_err(failure)??;
    check(
        ended == 19,
        "The dial limit ended the call with another cause",
    )?;
    Wait::Ready
        .bounded(call.close())
        .await
        .map_err(failure)?
        .map_err(failure)
}

fn combine(
    result: Result<(), ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<(), ControlError> {
    match (result, cleanup) {
        (Err(error), Err(cleanup)) => Err(failure(format!("{error}; cleanup failed: {cleanup}"))),
        (result, Ok(())) | (Ok(()), result) => result,
    }
}

#[derive(Clone, Copy)]
enum Scenario {
    LateAnswer,
    RingingDialLimit,
    SilentDialLimit,
}

#[tokio::test]
async fn the_sdk_cancels_a_udp_call_and_a_late_answer_cannot_reconnect_it(
) -> Result<(), ControlError> {
    served(Scenario::LateAnswer).await
}

#[tokio::test]
async fn a_ringing_call_past_its_dial_limit_receives_cancel_and_ends_with_no_answer(
) -> Result<(), ControlError> {
    served(Scenario::RingingDialLimit).await
}

#[tokio::test]
async fn a_silent_call_past_its_dial_limit_ends_with_no_answer_and_sends_no_cancel(
) -> Result<(), ControlError> {
    served(Scenario::SilentDialLimit).await
}

async fn served(scenario: Scenario) -> Result<(), ControlError> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let peer = Peer::bind(ip, ip).await?;
    let directory = tempfile::tempdir().map_err(failure)?;
    let sip = format!("[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[phone]\ntype=aor\ncontact=sip:phone@{}\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=phone\nallow=ulaw\n", peer.contact()?);
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        &sip,
        "",
        "",
    )
    .map_err(failure)?;
    config.http.bind.set_port(0);
    config.rtp.start = 0;
    config.rtp.end = 0;
    let server = Server::bind(config, 8, directory.path().to_owned()).await?;
    let client = Client::new(
        &format!("http://{}", server.local_addr()?),
        "app",
        "test-password",
    )
    .map_err(failure)?;
    let controller = server.controller();
    let mut running = tokio::spawn(server.run());
    let result = async {
        let mut events = client.subscribe(&id("app")?).await.map_err(failure)?;
        let result = match scenario {
            Scenario::LateAnswer => exchange(&client, &mut events, &peer).await,
            Scenario::RingingDialLimit => past_its_dial_limit(&mut events, &peer, true).await,
            Scenario::SilentDialLimit => past_its_dial_limit(&mut events, &peer, false).await,
        };
        let cleanup = match Wait::Ready.bounded(events.shutdown()).await {
            Ok(result) => result.map_err(failure),
            Err(error) => Err(failure(error)),
        };
        combine(result, cleanup)
    }
    .await;
    controller.shutdown();
    let cleanup = match Wait::Ready.bounded(&mut running).await {
        Ok(result) => result.map_err(failure).and_then(|result| result),
        Err(error) => {
            running.abort();
            match running.await {
                Ok(result) => combine(Err(failure(error)), result),
                Err(error) if error.is_cancelled() => {
                    Err(ControlError::Invalid("Server shutdown timed out"))
                }
                Err(error) => Err(failure(error)),
            }
        }
    };
    combine(result, cleanup)
}
