use crate::media::MediaHub;
use crate::signaling::{Signaling, SipEvent};
use crate::{CallKey, ControlError, Controller, Effect};
use rsipstack::sip::{HeadersExt, Method, Request, SipMessage};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_core::TransferToken;
use rusterisk_sip::{Outbound, Output, ReferKey, ReferProgress};
use std::net::SocketAddr;
use tokio::sync::mpsc::error::TryRecvError;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn request(packet: &[u8]) -> Result<Request, ControlError> {
    match SipMessage::try_from(packet).map_err(failure)? {
        SipMessage::Request(request) => Ok(request),
        SipMessage::Response(_) => Err(ControlError::Invalid("Expected a request")),
    }
}

fn receipt(packet: &Request, code: u16) -> Result<Vec<u8>, ControlError> {
    Ok(format!("SIP/2.0 {code} Result\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n", packet.via_header().map_err(failure)?.value(), packet.from_header().map_err(failure)?.value(), packet.to_header().map_err(failure)?.value(), packet.call_id_header().map_err(failure)?.value(), packet.cseq_header().map_err(failure)?.value()).into_bytes())
}

fn answer(invite: &Request) -> Result<Vec<u8>, ControlError> {
    let body = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=call\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 10002 RTP/AVP 0\r\na=sendrecv\r\n";
    Ok(format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=remote\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:alice@127.0.0.1:5061;transport=tcp>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", invite.via_header().map_err(failure)?.value(), invite.from_header().map_err(failure)?.value(), invite.to_header().map_err(failure)?.value(), invite.call_id_header().map_err(failure)?.value(), invite.cseq_header().map_err(failure)?.value(), body.len()).into_bytes())
}

fn accepted(packets: &mut tokio::sync::mpsc::Receiver<Vec<u8>>) -> Result<Request, ControlError> {
    let mut response = false;
    let mut notification = None;
    for _ in 0..2 {
        let bytes = packets.try_recv().map_err(failure)?;
        match SipMessage::try_from(bytes.as_slice()).map_err(failure)? {
            SipMessage::Response(received) => {
                if response || received.status_code.code() != 202 {
                    return Err(ControlError::Invalid("Expected one REFER acceptance"));
                }
                response = true;
            }
            SipMessage::Request(received) => {
                if received.method != Method::Notify || notification.replace(received).is_some() {
                    return Err(ControlError::Invalid("Expected one initial NOTIFY"));
                }
            }
        }
    }
    if !response {
        return Err(ControlError::Invalid("Missing REFER acceptance"));
    }
    notification.ok_or(ControlError::Invalid("Missing initial NOTIFY"))
}

fn refer(
    signaling: &mut Signaling,
    source: &CallKey,
    peer: SocketAddr,
) -> Result<(ReferKey, Output, Vec<u8>), ControlError> {
    let engine = &mut signaling.listeners[0].engine;
    let started = engine
        .originate(
            Outbound {
                id: source.id.as_str().into(),
                generation: source.generation,
                endpoint: "alice".into(),
                caller: "100".into(),
                local_rtp: "127.0.0.1:10000".parse().map_err(failure)?,
            },
            0,
        )
        .map_err(failure)?;
    let invite = request(
        &started
            .datagrams
            .first()
            .ok_or(ControlError::Capacity)?
            .bytes,
    )?;
    let from = invite.from_header().map_err(failure)?.value();
    let to = format!(
        "{};tag=remote",
        invite.to_header().map_err(failure)?.value()
    );
    let call_id = invite.call_id_header().map_err(failure)?.value();
    let answered = engine
        .receive(&answer(&invite)?, peer, 1)
        .map_err(failure)?;
    if !answered.events.iter().any(|event| matches!(event, SipEvent::Answered { id, generation, .. } if id == source.id.as_str() && *generation == source.generation)) {
        return Err(ControlError::Invalid("Missing established source dialog"));
    }
    let packet = format!("REFER sip:100@localhost SIP/2.0\r\nVia: SIP/2.0/TCP 127.0.0.1:5061;branch=z9hG4bK-refer;rport\r\nFrom: {to}\r\nTo: {from}\r\nCall-ID: {call_id}\r\nCSeq: 3 REFER\r\nContact: <sip:alice@127.0.0.1:5061;transport=tcp>\r\nRefer-To: <sip:200@localhost>\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n").into_bytes();
    let requested = engine.receive(&packet, peer, 5).map_err(failure)?;
    let reference = requested
        .events
        .iter()
        .find_map(|event| {
            if let SipEvent::BlindTransferRequested {
                id,
                generation,
                refer,
            } = event
            {
                (id == source.id.as_str() && *generation == source.generation)
                    .then_some(refer.key())
            } else {
                None
            }
        })
        .ok_or(ControlError::Invalid("Missing dialog REFER"))?;
    Ok((reference, requested, packet))
}

fn status(packet: &[u8], code: u16) -> Result<(), ControlError> {
    match SipMessage::try_from(packet).map_err(failure)? {
        SipMessage::Response(response) if response.status_code.code() == code => Ok(()),
        SipMessage::Response(response) => Err(ControlError::Adapter(format!(
            "Expected SIP {code}, received {}",
            response.status_code.code()
        ))),
        SipMessage::Request(_) => Err(ControlError::Invalid("Expected a response")),
    }
}

enum Usage {
    Active,
    Settled,
}

async fn cached_reply_flow(usage: Usage) -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test\n",
        "[stream]\ntype=transport\nprotocol=tcp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=alice\npassword=password\n[aor]\ntype=aor\ncontact=sip:alice@127.0.0.1:5061;transport=tcp\n[alice]\ntype=endpoint\ntransport=stream\nauth=auth\naors=aor\ncontext=internal\nallow=ulaw\n", "", ""
    ).map_err(failure)?;
    let (controller, _runner, effects) = Controller::new(config.clone(), 8);
    let (media, _hub, notices) = MediaHub::new(8);
    let mut signaling = Signaling::bind(config, controller, effects, media, notices).await?;
    let address = signaling.network.local_addr(0)?;
    let source = CallKey {
        id: ChannelId::try_from("refer-flow".to_owned())?,
        generation: 7,
    };
    let peer = "127.0.0.1:5061".parse().map_err(failure)?;
    let result: Result<(), ControlError> = async {
        let mut original = signaling.network.reserve(0, peer)?;
        let (reference, requested, packet) = refer(&mut signaling, &source, peer)?;
        signaling.output(0, requested, 5).await?;
        match original.packets.try_recv() {
            Err(TryRecvError::Empty) => {}
            Ok(bytes) => {
                status(&bytes, 202)?;
                return Err(ControlError::Invalid(
                    "REFER replied before Controller admission",
                ));
            }
            Err(error) => return Err(failure(error)),
        }
        let token = TransferToken::new(1);
        let mut stale_source = source.clone();
        stale_source.generation += 1;
        signaling.effect(Effect::ReferProgress { source: stale_source, refer: reference, token, progress: ReferProgress::Trying }, 6).await?;
        if !matches!(original.packets.try_recv(), Err(TryRecvError::Empty)) {
            return Err(ControlError::Invalid("Stale source changed REFER progress"));
        }
        signaling
            .effect(
                Effect::ReferProgress {
                    source: source.clone(),
                    refer: reference,
                    token,
                    progress: ReferProgress::Trying,
                },
                6,
            )
            .await?;
        let initial = accepted(&mut original.packets)?;
        let acknowledged = signaling.listeners[0]
            .engine
            .receive(&receipt(&initial, 200)?, peer, 7)
            .map_err(failure)?;
        signaling.output(0, acknowledged, 7).await?;
        signaling.effect(Effect::ReferProgress { source: source.clone(), refer: reference, token: TransferToken::new(2), progress: ReferProgress::Answered }, 8).await?;
        if !matches!(original.packets.try_recv(), Err(TryRecvError::Empty)) {
            return Err(ControlError::Invalid("Stale token changed REFER progress"));
        }
        match usage {
            Usage::Active => signaling.effect(Effect::ReferProgress { source: source.clone(), refer: reference, token, progress: ReferProgress::Ringing }, 8).await?,
            Usage::Settled => {
                signaling.effect(Effect::ReferProgress { source: source.clone(), refer: reference, token, progress: ReferProgress::Answered }, 8).await?;
                let finished = request(&original.packets.try_recv().map_err(failure)?)?;
                if finished.method != Method::Notify { return Err(ControlError::Invalid("Missing final NOTIFY")); }
                let settled = signaling.listeners[0].engine.receive(&receipt(&finished, 200)?, peer, 9).map_err(failure)?;
                if !settled.events.iter().any(|event| matches!(event, SipEvent::ReferSubscriptionEnded { refer } if *refer == reference)) {
                    return Err(ControlError::Invalid("Final NOTIFY did not settle its usage"));
                }
                signaling.output(0, settled, 9).await?;
            }
        }
        let replay = signaling.listeners[0]
            .engine
            .receive(&packet, peer, 10)
            .map_err(failure)?;
        signaling.output(0, replay, 10).await?;
        status(&original.packets.try_recv().map_err(failure)?, 202)?;
        signaling.network.ports.remove(&original.key);
        let mut replacement = signaling.network.reserve(0, peer)?;
        let target = "unrelated-target";
        let started = signaling.listeners[0].engine.originate(Outbound {
            id: target.into(), generation: 8, endpoint: "alice".into(), caller: "200".into(),
            local_rtp: "127.0.0.1:10004".parse().map_err(failure)?,
        }, 11).map_err(failure)?;
        signaling.output(0, started, 11).await?;
        let invite = request(&replacement.packets.try_recv().map_err(failure)?)?;
        if invite.method != Method::Invite { return Err(ControlError::Invalid("Independent INVITE was suppressed")); }
        let established = signaling.listeners[0].engine.receive(&answer(&invite)?, peer, 12).map_err(failure)?;
        if !established.events.iter().any(|event| matches!(event, SipEvent::Answered { id, generation: 8, .. } if id == target)) {
            return Err(ControlError::Invalid("Independent target did not answer"));
        }
        signaling.transmit(0, established.datagrams).await?;
        if request(&replacement.packets.try_recv().map_err(failure)?)?.method != Method::Ack {
            return Err(ControlError::Invalid("Independent ACK was suppressed"));
        }
        let replay = signaling.listeners[0]
            .engine
            .receive(&packet, peer, 13)
            .map_err(failure)?;
        signaling.output(0, replay, 13).await?;
        if !matches!(replacement.packets.try_recv(), Err(TryRecvError::Empty)) {
            return Err(ControlError::Invalid(
                "Cached REFER reached a replacement flow",
            ));
        }
        if signaling.listeners[0].engine.refer_identity(reference).is_some() {
            return Err(ControlError::Invalid("Closed flow kept an active REFER usage"));
        }
        let digit = rusterisk_core::DtmfDigit::from_event(1).ok_or(ControlError::Invalid("Missing digit"))?;
        let continued = signaling.listeners[0].engine.send_info(target, 8, digit, 100, 14).map_err(failure)?;
        signaling.transmit(0, continued.datagrams).await?;
        let info = request(&replacement.packets.try_recv().map_err(failure)?)?;
        if info.method != Method::Info { return Err(ControlError::Invalid("Independent target ended with the REFER")); }
        signaling.listeners[0].engine.receive(&receipt(&info, 200)?, peer, 15).map_err(failure)?;
        let register = b"REGISTER sip:localhost SIP/2.0\r\nVia: SIP/2.0/TCP 127.0.0.1:5061;branch=z9hG4bK-register;rport\r\nFrom: <sip:alice@localhost>;tag=register\r\nTo: <sip:alice@localhost>\r\nCall-ID: register-independent\r\nCSeq: 1 REGISTER\r\nContact: <sip:alice@127.0.0.1:5061;transport=tcp>\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n";
        let registration = signaling.listeners[0].engine.receive(register, peer, 1_000).map_err(failure)?;
        signaling.output(0, registration, 1_000).await?;
        status(&replacement.packets.try_recv().map_err(failure)?, 401)?;
        let expired = signaling.listeners[0].engine.tick(32_006).map_err(failure)?;
        signaling.output(0, expired, 32_006).await?;
        if !signaling.refer_routes.is_empty() { return Err(ControlError::Invalid("Expired REFER route remained owned")); }
        let registration = signaling.listeners[0].engine.receive(register, peer, 32_007).map_err(failure)?;
        signaling.output(0, registration, 32_007).await?;
        status(&replacement.packets.try_recv().map_err(failure)?, 401)?;
        Ok(())
    }
    .await;
    let cleanup = signaling.network.close().await;
    drop(signaling);
    let rebound = tokio::net::TcpListener::bind(address)
        .await
        .map_err(failure);
    let (_listener, rebound) = match rebound {
        Ok(listener) => {
            let checked = listener.local_addr().map_err(failure).and_then(|actual| {
                if actual == address {
                    Ok(())
                } else {
                    Err(ControlError::Invalid("Listener cleanup changed"))
                }
            });
            (Some(listener), checked)
        }
        Err(error) => (None, Err(error)),
    };
    crate::signaling::cleanup::combine(crate::signaling::cleanup::combine(result, cleanup), rebound)
}

#[tokio::test]
async fn cached_refer_replies_keep_their_original_flow_after_subscription_completion(
) -> Result<(), ControlError> {
    cached_reply_flow(Usage::Settled).await
}

#[tokio::test]
async fn closing_original_flow_releases_active_refer_without_ending_another_dialog(
) -> Result<(), ControlError> {
    cached_reply_flow(Usage::Active).await
}
