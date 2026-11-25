use super::{incoming_id, incoming_request, invite, request};
use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    HeadersExt, Method, SipMessage, Uri,
};
use rusterisk_config::{Config, Protocol};
use rusterisk_sip::{
    Datagram, DatagramOrigin, Engine, Outbound, OutboundProfile, Output, SipError, SipEvent,
    StreamFlowId,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[path = "flow_provenance/admission.rs"]
mod admission;
#[path = "flow_provenance/bindings.rs"]
mod bindings;
#[path = "flow_provenance/outgoing.rs"]
mod outgoing;
#[path = "flow_provenance/refer.rs"]
mod refer;

const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5061);

fn settings(protocol: Protocol) -> Result<Config, SipError> {
    let (name, tls, contact) = match protocol {
        Protocol::Tcp => ("tcp", "", "sip:phone@127.0.0.1:5061\\;transport=tcp"),
        Protocol::Tls => (
            "tls",
            "cert_file=certificate.pem\npriv_key_file=private-key.pem\n",
            "sips:phone@127.0.0.1:5061\\;transport=tcp",
        ),
        Protocol::Udp | Protocol::Ws | Protocol::Wss => return Err(SipError::Unsupported),
    };
    Ok(Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        &format!("[stream]\ntype=transport\nprotocol={name}\nbind=127.0.0.1:5060\n{tls}[a]\ntype=auth\nusername=phone\npassword=password\nrealm=rusterisk\n[aor]\ntype=aor\ncontact={contact}\n[phone]\ntype=endpoint\ntransport=stream\nauth=a\noutbound_auth=a\naors=aor\ncontext=internal\nallow=ulaw,alaw,opus\n"),
        "",
        "",
    )?)
}

fn engine(protocol: Protocol) -> Result<Engine, SipError> {
    Ok(Engine::new(settings(protocol)?, [37; 32]).with_transport(protocol))
}

fn transported(wire: &str, protocol: Protocol) -> Result<String, SipError> {
    let token = match protocol {
        Protocol::Tcp => "TCP",
        Protocol::Tls => "TLS",
        Protocol::Udp | Protocol::Ws | Protocol::Wss => return Err(SipError::Unsupported),
    };
    Ok(wire.replace("SIP/2.0/UDP", &format!("SIP/2.0/{token}")))
}

fn invitation(
    protocol: Protocol,
    authorization: &str,
    sequence: u32,
    call: &str,
) -> Result<String, SipError> {
    transported(
        &invite(authorization, sequence)
            .replace("test-call", call)
            .replace("tag=caller", &format!("tag={call}")),
        protocol,
    )
}

fn first(output: &Output) -> Result<&Datagram, SipError> {
    output.datagrams.first().ok_or(SipError::InvalidPacket)
}

fn scoped(output: &Output, flow: StreamFlowId) {
    assert!(!output.datagrams.is_empty());
    assert!(output
        .datagrams
        .iter()
        .all(|packet| packet.peer == PEER && packet.flow() == Some(flow)));
}

fn authenticate(challenge: &[u8]) -> Result<String, SipError> {
    let SipMessage::Response(response) = SipMessage::try_from(challenge)? else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(response.status_code.code(), 401);
    let challenge = WwwAuthenticate::parse(
        response
            .www_authenticate_header()
            .ok_or(SipError::InvalidPacket)?
            .value(),
    )?;
    let uri = Uri::try_from("sip:100@localhost")?;
    let qop = AuthQop::Auth {
        cnonce: "flow-client".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username: "phone",
        password: "password",
        nonce: &challenge.nonce,
        uri: &uri,
        realm: &challenge.realm,
        method: &Method::Invite,
        qop: Some(&qop),
        algorithm: Algorithm::Md5,
    }
    .compute();
    Ok(format!("Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"sip:100@localhost\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"flow-client\", nc=00000001\r\n", challenge.realm, challenge.nonce))
}

struct Dialog {
    id: String,
    invitation: String,
    answer: Vec<u8>,
}

fn establish(
    engine: &mut Engine,
    protocol: Protocol,
    flow: StreamFlowId,
    call: &str,
    now: u64,
) -> Result<Dialog, SipError> {
    let challenged = engine.receive_on_flow(
        invitation(protocol, "", 1, call)?.as_bytes(),
        PEER,
        flow,
        now,
    )?;
    scoped(&challenged, flow);
    let authorization = authenticate(&first(&challenged)?.bytes)?;
    let invitation = invitation(protocol, &authorization, 2, call)?;
    let admitted = engine.receive_on_flow(invitation.as_bytes(), PEER, flow, now + 1)?;
    scoped(&admitted, flow);
    let id = incoming_id(&admitted)?;
    let answered = engine.answer(&id, 7, "127.0.0.1:10000".parse()?, now + 2)?;
    scoped(&answered, flow);
    let answer = first(&answered)?.bytes.clone();
    let ack = transported(
        std::str::from_utf8(&incoming_request(&answer, "ACK", 2)?)?,
        protocol,
    )?;
    let accepted = engine.receive_on_flow(ack.as_bytes(), PEER, flow, now + 3)?;
    assert!(accepted.datagrams.is_empty());
    assert!(accepted.events.is_empty());
    Ok(Dialog {
        id,
        invitation,
        answer,
    })
}

fn dialog_request(
    dialog: &Dialog,
    protocol: Protocol,
    method: &str,
    sequence: u32,
    extra: &str,
) -> Result<String, SipError> {
    transported(
        &std::str::from_utf8(&incoming_request(&dialog.answer, method, sequence)?)?.replacen(
            "Content-Length:",
            &format!("{extra}Content-Length:"),
            1,
        ),
        protocol,
    )
}

fn originate(
    engine: &mut Engine,
    protocol: Protocol,
    flow: StreamFlowId,
    id: &str,
    generation: u64,
    now: u64,
) -> Result<Output, SipError> {
    let profile = OutboundProfile::capture(&settings(protocol)?, "phone")?;
    let prepared = engine.prepare_outbound(profile, now)?;
    let output = engine.originate_prepared_on_flow(
        Outbound {
            id: id.into(),
            generation,
            endpoint: "phone".into(),
            caller: "100".into(),
            local_rtp: "127.0.0.1:10000".parse()?,
        },
        prepared,
        Some(PEER),
        None,
        flow,
        now,
    )?;
    scoped(&output, flow);
    Ok(output)
}

fn answer(protocol: Protocol, invitation: &[u8], tag: &str) -> Result<Vec<u8>, SipError> {
    let bytes = super::answered(invitation)?;
    let contact = match protocol {
        Protocol::Tcp => "sip:phone@127.0.0.1:5061;transport=tcp",
        Protocol::Tls => "sips:phone@127.0.0.1:5061;transport=tcp",
        Protocol::Udp | Protocol::Ws | Protocol::Wss => return Err(SipError::Unsupported),
    };
    Ok(std::str::from_utf8(&bytes)?
        .replace("tag=callee", &format!("tag={tag}"))
        .replace("sip:phone@127.0.0.1:5061", contact)
        .into_bytes())
}

#[test]
fn another_flow_cannot_spend_a_nonce_or_replay_its_cached_admission() -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let mut engine = engine(protocol)?;
        let original = StreamFlowId::new(101)?;
        let another = StreamFlowId::new(102)?;
        let wire = invitation(protocol, "", 1, "nonce-flow")?;
        let challenged = engine.receive_on_flow(wire.as_bytes(), PEER, original, 0)?;
        scoped(&challenged, original);
        let duplicate = engine.receive_on_flow(wire.as_bytes(), PEER, another, 1)?;
        scoped(&duplicate, another);
        assert!(first(&duplicate)?.bytes.starts_with(b"SIP/2.0 401 "));
        assert!(first(&duplicate)?.bytes != first(&challenged)?.bytes);
        assert!(duplicate.events.is_empty());
        let repeated = engine.receive_on_flow(wire.as_bytes(), PEER, original, 2)?;
        assert!(first(&repeated)?.bytes == first(&challenged)?.bytes);
        scoped(&repeated, original);
        let authorization = authenticate(&first(&challenged)?.bytes)?;
        let signed = invitation(protocol, &authorization, 2, "nonce-flow")?;
        let rejected = engine.receive_on_flow(signed.as_bytes(), PEER, another, 3)?;
        scoped(&rejected, another);
        assert!(first(&rejected)?.bytes.starts_with(b"SIP/2.0 401 "));
        assert!(rejected.events.is_empty());
        let admitted = engine.receive_on_flow(signed.as_bytes(), PEER, original, 4)?;
        scoped(&admitted, original);
        let id = incoming_id(&admitted)?;
        let rejected = engine.receive_on_flow(signed.as_bytes(), PEER, another, 5)?;
        scoped(&rejected, another);
        assert!(first(&rejected)?.bytes.starts_with(b"SIP/2.0 401 "));
        assert!(rejected.events.is_empty());
        let repeated = engine.receive_on_flow(signed.as_bytes(), PEER, original, 6)?;
        scoped(&repeated, original);
        assert!(first(&repeated)?.bytes == first(&admitted)?.bytes);
        assert!(repeated.events.is_empty());
        assert!(engine.pending(&id).is_some());
    }
    Ok(())
}

#[test]
fn a_dialog_and_its_cached_reply_survive_the_other_flow_disconnecting() -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let mut engine = engine(protocol)?;
        let original = StreamFlowId::new(201)?;
        let another = StreamFlowId::new(202)?;
        let first_dialog = establish(&mut engine, protocol, original, "first-dialog", 0)?;
        let second = establish(&mut engine, protocol, another, "second-dialog", 4)?;
        let bye = dialog_request(&first_dialog, protocol, "BYE", 3, "")?;
        let rejected = engine.receive_on_flow(bye.as_bytes(), PEER, another, 8)?;
        scoped(&rejected, another);
        assert!(first(&rejected)?.bytes.starts_with(b"SIP/2.0 481 "));
        assert!(rejected.events.is_empty());
        assert!(engine.active_session(&first_dialog.id, 7).is_some());
        assert!(engine.active_session(&second.id, 7).is_some());
        let closed = engine.disconnected_flow(original);
        assert!(closed.datagrams.is_empty());
        assert!(
            matches!(closed.events.as_slice(), [SipEvent::Ended { id, generation: 7, cause: 41 }] if id == &first_dialog.id)
        );
        assert!(engine.active_session(&first_dialog.id, 7).is_none());
        assert!(engine.active_session(&second.id, 7).is_some());
        assert!(engine.disconnected_flow(original).events.is_empty());
        let repeated = engine.receive_on_flow(second.invitation.as_bytes(), PEER, another, 9)?;
        scoped(&repeated, another);
        assert!(first(&repeated)?.bytes == second.answer);
        assert!(repeated.events.is_empty());
        let ended = engine.cancel(&second.id, 7, 10)?;
        scoped(&ended, another);
        assert_eq!(request(&first(&ended)?.bytes)?.method, Method::Bye);
        assert_eq!(first(&ended)?.origin(), DatagramOrigin::Direct);
    }
    Ok(())
}
