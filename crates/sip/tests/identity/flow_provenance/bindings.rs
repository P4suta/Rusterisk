use super::{answer, authenticate, first, incoming_id, invitation, request, settings, PEER};
use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
        untyped::ToTypedHeader,
    },
    services::DigestGenerator,
    HeadersExt, Method, SipMessage, Uri,
};
use rusterisk_config::{Config, Protocol};
use rusterisk_sip::{
    Destination, Engine, Outbound, OutboundProfile, Output, PreparedOutbound, SipError, SipEvent,
    StreamFlowId,
};
use std::net::SocketAddr;

#[path = "bindings/admission.rs"]
mod admission;

fn configuration(protocol: Protocol) -> Result<Config, SipError> {
    let mut config = settings(protocol)?;
    config
        .aors
        .iter_mut()
        .find(|aor| aor.name == "aor")
        .ok_or(SipError::InvalidPacket)?
        .max_contacts = 2;
    Ok(config)
}

fn changed_endpoint_aor(starting: &Config) -> Result<Config, SipError> {
    let mut current = starting.clone();
    let endpoint = current
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.name == "phone")
        .ok_or(SipError::InvalidPacket)?;
    let mut registrar = endpoint.clone();
    registrar.name = "registration-principal".into();
    endpoint.aors = ["current-aor".into()].into_iter().collect();
    current.endpoints.insert(0, registrar);
    let mut aor = current
        .aors
        .iter()
        .find(|aor| aor.name == "aor")
        .cloned()
        .ok_or(SipError::InvalidPacket)?;
    aor.name = "current-aor".into();
    current.aors.push(aor);
    Ok(current)
}

fn scheme(protocol: Protocol) -> Result<(&'static str, &'static str), SipError> {
    match protocol {
        Protocol::Tcp => Ok(("sip", "TCP")),
        Protocol::Tls => Ok(("sips", "TLS")),
        Protocol::Udp | Protocol::Ws | Protocol::Wss => Err(SipError::Unsupported),
    }
}

fn contact(protocol: Protocol, user: &str) -> Result<String, SipError> {
    let (scheme, _) = scheme(protocol)?;
    Ok(format!("{scheme}:{user}@registered.invalid;transport=tcp"))
}

fn routed(output: &Output, peer: SocketAddr, flow: StreamFlowId) {
    assert!(!output.datagrams.is_empty());
    assert!(output
        .datagrams
        .iter()
        .all(|packet| packet.peer == peer && packet.flow() == Some(flow)));
}

fn registration(
    protocol: Protocol,
    peer: SocketAddr,
    label: &str,
    sequence: u32,
    contacts: &[(&str, u32, u16)],
    authorization: &str,
) -> Result<String, SipError> {
    let (scheme, transport) = scheme(protocol)?;
    let contacts: String = contacts
        .iter()
        .map(|(uri, expires, quality)| {
            format!(
                "Contact: <{uri}>;expires={expires};q={}.{:03}\r\n",
                quality / 1000,
                quality % 1000
            )
        })
        .collect();
    Ok(format!("REGISTER {scheme}:registrar.invalid SIP/2.0\r\nVia: SIP/2.0/{transport} {peer};branch=z9hG4bK-{label}-{sequence};rport\r\nFrom: <{scheme}:aor@registrar.invalid>;tag={label}\r\nTo: <{scheme}:aor@registrar.invalid>\r\nCall-ID: registration-{label}\r\nCSeq: {sequence} REGISTER\r\nMax-Forwards: 70\r\n{contacts}{authorization}Content-Length: 0\r\n\r\n"))
}

fn registration_authorization(protocol: Protocol, bytes: &[u8]) -> Result<String, SipError> {
    let SipMessage::Response(response) = SipMessage::try_from(bytes)? else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(response.status_code.code(), 401);
    let challenge = WwwAuthenticate::parse(
        response
            .www_authenticate_header()
            .ok_or(SipError::InvalidPacket)?
            .value(),
    )?;
    let (scheme, _) = scheme(protocol)?;
    let uri = Uri::try_from(format!("{scheme}:registrar.invalid").as_str())?;
    let qop = AuthQop::Auth {
        cnonce: "binding-client".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username: "phone",
        password: "password",
        nonce: &challenge.nonce,
        uri: &uri,
        realm: &challenge.realm,
        method: &Method::Register,
        qop: Some(&qop),
        algorithm: Algorithm::Md5,
    }
    .compute();
    Ok(format!("Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"binding-client\", nc=00000001\r\n", challenge.realm, challenge.nonce))
}

fn register(
    engine: &mut Engine,
    protocol: Protocol,
    peer: SocketAddr,
    flow: StreamFlowId,
    label: &str,
    contacts: &[(&str, u32, u16)],
    now: u64,
) -> Result<(), SipError> {
    let wire = registration(protocol, peer, label, 1, contacts, "")?;
    let challenged = engine.receive_on_flow(wire.as_bytes(), peer, flow, now)?;
    routed(&challenged, peer, flow);
    let authorization = registration_authorization(protocol, &first(&challenged)?.bytes)?;
    let wire = registration(protocol, peer, label, 2, contacts, &authorization)?;
    let registered = engine.receive_on_flow(wire.as_bytes(), peer, flow, now + 1)?;
    routed(&registered, peer, flow);
    let SipMessage::Response(response) =
        SipMessage::try_from(first(&registered)?.bytes.as_slice())?
    else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(response.status_code.code(), 200);
    Ok(())
}

fn prepare(
    engine: &Engine,
    starting: &Config,
    peer: SocketAddr,
    now: u64,
) -> Result<PreparedOutbound, SipError> {
    let prepared = engine.prepare_outbound(OutboundProfile::capture(starting, "phone")?, now)?;
    assert!(matches!(prepared.destination(), Destination::Registered(found) if *found == peer));
    Ok(prepared)
}

fn start(
    engine: &mut Engine,
    prepared: PreparedOutbound,
    id: &str,
    flow: StreamFlowId,
    now: u64,
) -> Result<Output, SipError> {
    engine.originate_prepared_on_flow(
        Outbound {
            id: id.into(),
            generation: 7,
            endpoint: "phone".into(),
            caller: "presentation-only".into(),
            local_rtp: "127.0.0.1:10000".parse()?,
        },
        prepared,
        None,
        None,
        flow,
        now,
    )
}

fn invitation_target(output: &Output, uri: &str) -> Result<(), SipError> {
    let request = request(&first(output)?.bytes)?;
    assert_eq!(request.method, Method::Invite);
    assert_eq!(request.uri.to_string(), Uri::try_from(uri)?.to_string());
    assert_eq!(
        request.from_header()?.typed()?.uri.user(),
        Some("presentation-only")
    );
    assert!(request.authorization_header().is_none());
    Ok(())
}

enum Replacement {
    Contact,
    Flow,
    Peer,
}

#[test]
fn static_and_legacy_registered_preparations_have_no_scoped_flow() -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let starting = configuration(protocol)?;
        let mut engine = Engine::new(starting.clone(), [79; 32]).with_transport(protocol);
        let profile = OutboundProfile::capture(&starting, "phone")?;
        let prepared = engine.prepare_outbound(profile, 0)?;
        assert!(matches!(prepared.destination(), Destination::Static(_)));
        assert_eq!(prepared.flow(), None);
        let uri = contact(protocol, "legacy")?;
        let contacts = [(uri.as_str(), 60, 1000)];
        let wire = registration(protocol, PEER, "legacy", 1, &contacts, "")?;
        let challenged = engine.receive(wire.as_bytes(), PEER, 0)?;
        assert!(challenged
            .datagrams
            .iter()
            .all(|packet| packet.flow().is_none()));
        let authorization = registration_authorization(protocol, &first(&challenged)?.bytes)?;
        let wire = registration(protocol, PEER, "legacy", 2, &contacts, &authorization)?;
        let registered = engine.receive(wire.as_bytes(), PEER, 1)?;
        let SipMessage::Response(response) =
            SipMessage::try_from(first(&registered)?.bytes.as_slice())?
        else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(response.status_code.code(), 200);
        assert!(registered
            .datagrams
            .iter()
            .all(|packet| packet.flow().is_none()));
        let prepared = prepare(&engine, &starting, PEER, 2)?;
        assert_eq!(prepared.flow(), None);
    }
    Ok(())
}

#[test]
fn prepared_registered_routes_reject_replaced_bindings_without_current_settings_fallback(
) -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        for replacement in [Replacement::Contact, Replacement::Flow, Replacement::Peer] {
            let starting = configuration(protocol)?;
            let mut engine =
                Engine::new(changed_endpoint_aor(&starting)?, [67; 32]).with_transport(protocol);
            let original = StreamFlowId::new(801)?;
            let another = StreamFlowId::new(802)?;
            let original_uri = contact(protocol, "original")?;
            register(
                &mut engine,
                protocol,
                PEER,
                original,
                "original",
                &[(&original_uri, 60, 1000)],
                0,
            )?;
            assert_eq!(engine.registered_peer("phone", 2), None);
            let captured = prepare(&engine, &starting, PEER, 2)?;
            assert_eq!(captured.flow(), Some(original));
            let changed_uri = contact(protocol, "replacement")?;
            let (peer, flow, uri) = match replacement {
                Replacement::Contact => {
                    register(
                        &mut engine,
                        protocol,
                        PEER,
                        original,
                        "contact-change",
                        &[(&original_uri, 0, 1000), (&changed_uri, 60, 1000)],
                        3,
                    )?;
                    (PEER, original, changed_uri.as_str())
                }
                Replacement::Flow | Replacement::Peer => {
                    let peer = match replacement {
                        Replacement::Flow => PEER,
                        Replacement::Peer => "127.0.0.1:5062".parse()?,
                        Replacement::Contact => return Err(SipError::InvalidPacket),
                    };
                    register(
                        &mut engine,
                        protocol,
                        peer,
                        another,
                        "route-change",
                        &[(&original_uri, 60, 1000)],
                        3,
                    )?;
                    (peer, another, original_uri.as_str())
                }
            };
            assert!(start(&mut engine, captured, "stale-binding", original, 5).is_err());
            assert!(engine.active_session("stale-binding", 7).is_none());
            let current = prepare(&engine, &starting, peer, 6)?;
            let offered = start(&mut engine, current, "current-binding", flow, 6)?;
            routed(&offered, peer, flow);
            invitation_target(&offered, uri)?;
        }
    }
    Ok(())
}

#[test]
fn disconnecting_one_registered_flow_preserves_the_other_binding_and_call() -> Result<(), SipError>
{
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let starting = configuration(protocol)?;
        let mut engine = Engine::new(starting.clone(), [71; 32]).with_transport(protocol);
        let original = StreamFlowId::new(901)?;
        let another = StreamFlowId::new(902)?;
        let first_uri = contact(protocol, "disconnected")?;
        let second_uri = contact(protocol, "retained")?;
        for (flow, uri, id, quality, now) in [
            (original, &first_uri, "first-binding", 500, 0),
            (another, &second_uri, "second-binding", 1000, 4),
        ] {
            register(
                &mut engine,
                protocol,
                PEER,
                flow,
                id,
                &[(uri, 60, quality)],
                now,
            )?;
            let prepared = prepare(&engine, &starting, PEER, now + 2)?;
            let offered = start(&mut engine, prepared, id, flow, now + 2)?;
            routed(&offered, PEER, flow);
            invitation_target(&offered, uri)?;
            let accepted = engine.receive_on_flow(
                &answer(protocol, &first(&offered)?.bytes, id)?,
                PEER,
                flow,
                now + 3,
            )?;
            routed(&accepted, PEER, flow);
            assert!(engine.active_session(id, 7).is_some());
        }
        let closed = engine.disconnected_flow(original);
        assert!(closed.datagrams.is_empty());
        assert!(
            matches!(closed.events.as_slice(), [SipEvent::Ended { id, generation: 7, cause: 41 }] if id == "first-binding")
        );
        assert!(engine.active_session("first-binding", 7).is_none());
        assert!(engine.active_session("second-binding", 7).is_some());
        assert_eq!(engine.registered_peer("phone", 8), Some(PEER));
        let prepared = prepare(&engine, &starting, PEER, 8)?;
        let offered = start(&mut engine, prepared, "after-disconnect", another, 8)?;
        routed(&offered, PEER, another);
        invitation_target(&offered, &second_uri)?;
        let ended = engine.cancel("second-binding", 7, 9)?;
        routed(&ended, PEER, another);
        assert_eq!(request(&first(&ended)?.bytes)?.method, Method::Bye);
        assert!(engine.disconnected_flow(original).events.is_empty());
    }
    Ok(())
}

#[test]
fn one_tick_retransmits_each_unacknowledged_answer_on_its_original_flow() -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let mut engine = Engine::new(settings(protocol)?, [73; 32]).with_transport(protocol);
        let original = StreamFlowId::new(1001)?;
        let another = StreamFlowId::new(1002)?;
        let mut answers = Vec::with_capacity(2);
        for (flow, call, now) in [(original, "tick-first", 0), (another, "tick-second", 4)] {
            let wire = invitation(protocol, "", 1, call)?;
            let challenged = engine.receive_on_flow(wire.as_bytes(), PEER, flow, now)?;
            routed(&challenged, PEER, flow);
            let authorization = authenticate(&first(&challenged)?.bytes)?;
            let wire = invitation(protocol, &authorization, 2, call)?;
            let admitted = engine.receive_on_flow(wire.as_bytes(), PEER, flow, now + 1)?;
            routed(&admitted, PEER, flow);
            let id = incoming_id(&admitted)?;
            let answered = engine.answer(&id, 7, "127.0.0.1:10000".parse()?, now + 2)?;
            routed(&answered, PEER, flow);
            answers.push((call, flow, first(&answered)?.bytes.clone()));
        }
        let retransmitted = engine.tick(506)?;
        assert!(retransmitted.events.is_empty());
        assert_eq!(retransmitted.datagrams.len(), 2);
        let mut seen = [false; 2];
        for packet in &retransmitted.datagrams {
            let SipMessage::Response(response) = SipMessage::try_from(packet.bytes.as_slice())?
            else {
                return Err(SipError::InvalidPacket);
            };
            assert_eq!(response.status_code.code(), 200);
            let call_id = response.call_id_header()?.value();
            let (index, (_, flow, bytes)) = answers
                .iter()
                .enumerate()
                .find(|(_, (call, _, _))| *call == call_id)
                .ok_or(SipError::InvalidPacket)?;
            assert!(!seen[index]);
            seen[index] = true;
            assert_eq!(packet.peer, PEER);
            assert_eq!(packet.flow(), Some(*flow));
            assert!(
                packet.bytes == *bytes,
                "Answer retransmission changed its original bytes"
            );
        }
        assert_eq!(seen, [true; 2]);
    }
    Ok(())
}
