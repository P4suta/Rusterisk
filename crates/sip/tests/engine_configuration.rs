use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
        untyped::ToTypedHeader,
    },
    services::DigestGenerator,
    HeadersExt, Method, Request, SipMessage,
};
use rusterisk_config::{Codec, Config, ConfigError, Protocol};
use rusterisk_sip::{
    Datagram, DatagramOrigin, DialogRefer, Engine, Outbound, OutboundProfile, Output,
    PreparedEngineConfiguration, ReferFailure, ReferProgress, SipError, SipEvent, StreamFlowId,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5061);
const SDP: &str = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=call\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 10002 RTP/AVP 0 8\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:8 PCMA/8000\r\na=sendrecv\r\n";
const REGISTERED: &str = "sip:registered@registered.invalid;transport=tcp";

fn token(protocol: Protocol) -> Result<&'static str, SipError> {
    match protocol {
        Protocol::Tcp => Ok("TCP"),
        Protocol::Tls => Ok("TLS"),
        Protocol::Udp | Protocol::Ws | Protocol::Wss => Err(SipError::Unsupported),
    }
}

fn configuration(protocol: Protocol, replacement: bool) -> Result<Config, SipError> {
    let (transport, scheme, files) = match protocol {
        Protocol::Tcp => ("tcp", "sip", ""),
        Protocol::Tls => (
            "tls",
            "sips",
            "cert_file=absent-certificate.pem\npriv_key_file=absent-private-key.pem\n",
        ),
        Protocol::Udp | Protocol::Ws | Protocol::Wss => return Err(SipError::Unsupported),
    };
    let (username, password, remote, remote_password, context, codecs, transfer, target) =
        if replacement {
            (
                "published-auth",
                "published-inbound",
                "published-remote",
                "published-outbound",
                "published-context",
                "alaw",
                "no",
                "published-user",
            )
        } else {
            (
                "starting-auth",
                "starting-inbound",
                "starting-remote",
                "starting-outbound",
                "starting-context",
                "ulaw,alaw",
                "yes",
                "starting-user",
            )
        };
    Ok(Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        &format!("[stream]\ntype=transport\nprotocol={transport}\nbind=127.0.0.1:5060\n{files}[incoming]\ntype=auth\nusername={username}\npassword={password}\nrealm=rusterisk\n[remote]\ntype=auth\nusername={remote}\npassword={remote_password}\nrealm=provider.test\n[aor]\ntype=aor\nmax_contacts=2\ncontact={scheme}:{target}@127.0.0.1:5061\\;transport=tcp\n[phone]\ntype=endpoint\ntransport=stream\nauth=incoming\noutbound_auth=remote\naors=aor\ncontext={context}\nallow={codecs}\nallow_transfer={transfer}\nrefer_blind_progress={transfer}\n"),
        "", "",
    )?)
}

fn first(output: &Output) -> Result<&Datagram, SipError> {
    output.datagrams.first().ok_or(SipError::InvalidPacket)
}

fn request(bytes: &[u8]) -> Result<Request, SipError> {
    match SipMessage::try_from(bytes)? {
        SipMessage::Request(request) => Ok(request),
        SipMessage::Response(_) => Err(SipError::InvalidPacket),
    }
}

fn status(output: &Output) -> Result<u16, SipError> {
    match SipMessage::try_from(first(output)?.bytes.as_slice())? {
        SipMessage::Response(response) => Ok(response.status_code.code()),
        SipMessage::Request(_) => Err(SipError::InvalidPacket),
    }
}

fn routed(output: &Output, flow: StreamFlowId) {
    assert!(!output.datagrams.is_empty());
    assert!(output
        .datagrams
        .iter()
        .all(|packet| packet.peer == PEER && packet.flow() == Some(flow)));
}

fn initial(
    protocol: Protocol,
    method: &Method,
    call: &str,
    sequence: u32,
    authorization: &str,
) -> Result<String, SipError> {
    let (uri, to, extra, body) = match method {
        Method::Invite => (
            "sip:100@localhost",
            "sip:100@localhost",
            "Content-Type: application/sdp\r\n".to_owned(),
            SDP,
        ),
        Method::Register => (
            "sip:registrar.invalid",
            "sip:aor@localhost",
            format!("Contact: <{REGISTERED}>;expires=60\r\n"),
            "",
        ),
        Method::Ack
        | Method::Bye
        | Method::Cancel
        | Method::Info
        | Method::Message
        | Method::Notify
        | Method::Options
        | Method::PRack
        | Method::Publish
        | Method::Refer
        | Method::Subscribe
        | Method::Update
        | Method::Other(_) => return Err(SipError::InvalidPacket),
    };
    Ok(format!("{method} {uri} SIP/2.0\r\nVia: SIP/2.0/{} {PEER};branch=z9hG4bK-{call}-{sequence};rport\r\nFrom: Display <{to}>;tag={call}\r\nTo: <{to}>\r\nCall-ID: {call}\r\nCSeq: {sequence} {method}\r\nMax-Forwards: 70\r\n{authorization}{extra}{}Content-Length: {}\r\n\r\n{body}", token(protocol)?, if method == &Method::Invite { "Contact: <sip:phone@127.0.0.1:5061;transport=tcp>\r\n" } else { "" }, body.len()))
}

fn authorization(
    challenged: &[u8],
    request: &Request,
    username: &str,
    password: &str,
) -> Result<String, SipError> {
    let SipMessage::Response(response) = SipMessage::try_from(challenged)? else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(response.status_code.code(), 401);
    let challenge = WwwAuthenticate::parse(
        response
            .www_authenticate_header()
            .ok_or(SipError::InvalidPacket)?
            .value(),
    )?;
    let qop = AuthQop::Auth {
        cnonce: "settings-client".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username,
        password,
        nonce: &challenge.nonce,
        uri: &request.uri,
        realm: &challenge.realm,
        method: &request.method,
        qop: Some(&qop),
        algorithm: Algorithm::Md5,
    }
    .compute();
    Ok(format!("Authorization: Digest username=\"{username}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"settings-client\", nc=00000001\r\n", challenge.realm, challenge.nonce, request.uri))
}

fn admitted(
    engine: &mut Engine,
    protocol: Protocol,
    method: Method,
    call: &str,
    flow: StreamFlowId,
    now: u64,
) -> Result<(Output, String), SipError> {
    let wire = initial(protocol, &method, call, 1, "")?;
    let challenged = engine.receive_on_flow(wire.as_bytes(), PEER, flow, now)?;
    routed(&challenged, flow);
    let authorization = authorization(
        &first(&challenged)?.bytes,
        &request(wire.as_bytes())?,
        "starting-auth",
        "starting-inbound",
    )?;
    let signed = initial(protocol, &method, call, 2, &authorization)?;
    let output = engine.receive_on_flow(signed.as_bytes(), PEER, flow, now + 1)?;
    routed(&output, flow);
    Ok((output, signed))
}

fn incoming(
    output: &Output,
    context: &str,
    identity: &str,
    codec: Codec,
) -> Result<String, SipError> {
    let [SipEvent::Incoming {
        id,
        endpoint,
        identity: actual_identity,
        caller,
        context: actual_context,
        session,
        ..
    }] = output.events.as_slice()
    else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(endpoint, "phone");
    assert_eq!(actual_identity, identity);
    assert_ne!(caller, actual_identity);
    assert_eq!(actual_context, context);
    assert_eq!(session.codec, codec);
    Ok(id.clone())
}

fn dialog_request(
    protocol: Protocol,
    answer: &[u8],
    method: Method,
    sequence: u32,
    extra: &str,
) -> Result<String, SipError> {
    let SipMessage::Response(response) = SipMessage::try_from(answer)? else {
        return Err(SipError::InvalidPacket);
    };
    let target = response.contact_header()?.typed()?.uri;
    Ok(format!("{method} {target} SIP/2.0\r\nVia: SIP/2.0/{} {PEER};branch=z9hG4bK-dialog-{}-{method}-{sequence};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} {method}\r\nMax-Forwards: 70\r\n{extra}Content-Length: 0\r\n\r\n", token(protocol)?, response.call_id_header()?.value(), response.from_header()?.value(), response.to_header()?.value(), response.call_id_header()?.value()))
}

fn refer(output: Output, id: &str) -> Result<DialogRefer, SipError> {
    let mut events = output.events.into_iter();
    let Some(SipEvent::BlindTransferRequested {
        id: actual,
        generation: 7,
        refer,
    }) = events.next()
    else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(actual, id);
    assert!(events.next().is_none());
    assert_eq!(refer.authority().context(), "starting-context");
    assert_eq!(
        refer.authority().authenticated_identity(),
        Some("starting-auth")
    );
    Ok(refer)
}

fn notification(output: &Output) -> Result<&Datagram, SipError> {
    for packet in &output.datagrams {
        match SipMessage::try_from(packet.bytes.as_slice())? {
            SipMessage::Request(request) if request.method == Method::Notify => return Ok(packet),
            SipMessage::Request(_) => return Err(SipError::InvalidPacket),
            SipMessage::Response(_) => {}
        }
    }
    Err(SipError::InvalidPacket)
}

fn response(
    invite: &[u8],
    code: u16,
    tag: &str,
    extra: &str,
    body: &str,
) -> Result<Vec<u8>, SipError> {
    let request = request(invite)?;
    let to = request.to_header()?.typed()?.with_tag(tag.into());
    Ok(format!("SIP/2.0 {code} Answer\r\nVia: {}\r\nFrom: {}\r\nTo: {to}\r\nCall-ID: {}\r\nCSeq: {}\r\n{extra}Content-Length: {}\r\n\r\n{body}", request.top_via_header()?.value(), request.from_header()?.value(), request.call_id_header()?.value(), request.cseq_header()?.value(), body.len()).into_bytes())
}

fn outbound(id: &str) -> Result<Outbound, SipError> {
    Ok(Outbound {
        id: id.into(),
        generation: 7,
        endpoint: "phone".into(),
        caller: "shown-caller".into(),
        local_rtp: "127.0.0.1:10000".parse()?,
        timeout_ms: Some(30_000),
    })
}

fn attribute<'a>(header: &'a str, name: &str) -> Result<&'a str, SipError> {
    header
        .strip_prefix("Digest ")
        .ok_or(SipError::InvalidPacket)?
        .split(',')
        .find_map(|field| {
            let (key, value) = field.trim().split_once('=')?;
            (key == name).then_some(value.trim_matches('"'))
        })
        .ok_or(SipError::InvalidPacket)
}

fn publish(engine: &mut Engine, prepared: PreparedEngineConfiguration) {
    engine.publish_configuration(prepared)
}

#[test]
fn preparation_preserves_typed_validation_errors_and_publication_returns_unit(
) -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let mut invalid = configuration(protocol, true)?;
        invalid.endpoints[0].codecs.clear();
        let expected = invalid
            .validate_runtime()
            .err()
            .ok_or(SipError::InvalidPacket)?;
        let prepared: Result<PreparedEngineConfiguration, ConfigError> =
            PreparedEngineConfiguration::prepare(invalid);
        match prepared {
            Err(actual) => assert_eq!(actual, expected),
            Ok(_) => return Err(SipError::InvalidPacket),
        }
        let mut engine =
            Engine::new(configuration(protocol, false)?, [41; 32]).with_transport(protocol);
        let prepared = PreparedEngineConfiguration::prepare(configuration(protocol, true)?)?;
        publish(&mut engine, prepared);
    }
    Ok(())
}

#[test]
fn publication_preserves_inbound_settlement_and_applies_new_initial_admission_settings(
) -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let mut engine =
            Engine::new(configuration(protocol, false)?, [43; 32]).with_transport(protocol);
        let pending_flow = StreamFlowId::new(21)?;
        let dialog_flow = StreamFlowId::new(22)?;
        let next_flow = StreamFlowId::new(23)?;
        let (pending, _) = admitted(
            &mut engine,
            protocol,
            Method::Invite,
            "pending-settings",
            pending_flow,
            0,
        )?;
        let pending_id = incoming(&pending, "starting-context", "starting-auth", Codec::Pcmu)?;
        let (admission, signed) = admitted(
            &mut engine,
            protocol,
            Method::Invite,
            "active-settings",
            dialog_flow,
            4,
        )?;
        let id = incoming(&admission, "starting-context", "starting-auth", Codec::Pcmu)?;
        let answered = engine.answer(&id, 7, "127.0.0.1:10000".parse()?, 6)?;
        let answer = first(&answered)?.bytes.clone();
        engine.receive_on_flow(
            dialog_request(protocol, &answer, Method::Ack, 2, "")?.as_bytes(),
            PEER,
            dialog_flow,
            7,
        )?;
        let refer_wire = dialog_request(
            protocol,
            &answer,
            Method::Refer,
            3,
            "Refer-To: <sip:200@target.invalid>\r\n",
        )?;
        let requested = refer(
            engine.receive_on_flow(refer_wire.as_bytes(), PEER, dialog_flow, 8)?,
            &id,
        )?;
        let accepted = engine.refer_progress(requested.key(), ReferProgress::Trying, 9)?;
        routed(&accepted, dialog_flow);
        let finished = engine.refer_progress(requested.key(), ReferProgress::Answered, 10)?;
        let final_notify = notification(&finished)?.bytes.clone();
        assert_eq!(
            notification(&finished)?.origin(),
            DatagramOrigin::Refer(requested.key())
        );
        let initial_next = initial(protocol, &Method::Invite, "published-admission", 1, "")?;
        let challenged = engine.receive_on_flow(initial_next.as_bytes(), PEER, next_flow, 11)?;
        let nonce = first(&challenged)?.bytes.clone();
        engine.publish_configuration(PreparedEngineConfiguration::prepare(configuration(
            protocol, true,
        )?)?);
        assert!(engine.pending(&pending_id).is_some());
        assert!(engine.active_session(&id, 7).is_some());
        let settled = engine.receive_on_flow(
            &response(&final_notify, 200, "active-settings", "", "")?,
            PEER,
            dialog_flow,
            12,
        )?;
        assert!(
            matches!(settled.events.as_slice(), [SipEvent::ReferSubscriptionEnded { refer }] if *refer == requested.key())
        );
        let replayed = engine.receive_on_flow(signed.as_bytes(), PEER, dialog_flow, 13)?;
        routed(&replayed, dialog_flow);
        assert!(
            first(&replayed)?.bytes == answer,
            "Publication replaced an existing server reply"
        );
        let pending_answer = engine.answer(&pending_id, 7, "127.0.0.1:10004".parse()?, 14)?;
        routed(&pending_answer, pending_flow);
        let SipMessage::Response(old_answer) =
            SipMessage::try_from(first(&pending_answer)?.bytes.as_slice())?
        else {
            return Err(SipError::InvalidPacket);
        };
        assert!(std::str::from_utf8(&old_answer.body)?.contains("RTP/AVP 0"));
        let next_refer = dialog_request(
            protocol,
            &answer,
            Method::Refer,
            4,
            "Refer-To: <sip:201@target.invalid>\r\n",
        )?;
        let continued = refer(
            engine.receive_on_flow(next_refer.as_bytes(), PEER, dialog_flow, 15)?,
            &id,
        )?;
        engine.refer_progress(
            continued.key(),
            ReferProgress::Rejected(ReferFailure::Cancelled),
            16,
        )?;
        let unsigned = initial(protocol, &Method::Invite, "published-admission", 2, "")?;
        let stale_auth = authorization(
            &nonce,
            &request(unsigned.as_bytes())?,
            "starting-auth",
            "starting-inbound",
        )?;
        let stale = initial(
            protocol,
            &Method::Invite,
            "published-admission",
            2,
            &stale_auth,
        )?;
        assert_eq!(
            status(&engine.receive_on_flow(stale.as_bytes(), PEER, next_flow, 17)?)?,
            401
        );
        let unsigned = initial(protocol, &Method::Invite, "published-admission", 3, "")?;
        let current_auth = authorization(
            &nonce,
            &request(unsigned.as_bytes())?,
            "published-auth",
            "published-inbound",
        )?;
        let current = initial(
            protocol,
            &Method::Invite,
            "published-admission",
            3,
            &current_auth,
        )?;
        let admitted = engine.receive_on_flow(current.as_bytes(), PEER, next_flow, 18)?;
        let current_id = incoming(
            &admitted,
            "published-context",
            "published-auth",
            Codec::Pcma,
        )?;
        let ended = engine.cancel(&id, 7, 19)?;
        routed(&ended, dialog_flow);
        assert_eq!(request(&first(&ended)?.bytes)?.method, Method::Bye);
        let answered = engine.answer(&current_id, 7, "127.0.0.1:10006".parse()?, 20)?;
        let bytes = &first(&answered)?.bytes;
        engine.receive_on_flow(
            dialog_request(protocol, bytes, Method::Ack, 3, "")?.as_bytes(),
            PEER,
            next_flow,
            21,
        )?;
        let refused = engine.receive_on_flow(
            dialog_request(
                protocol,
                bytes,
                Method::Refer,
                4,
                "Refer-To: <sip:202@target.invalid>\r\n",
            )?
            .as_bytes(),
            PEER,
            next_flow,
            22,
        )?;
        assert_eq!(status(&refused)?, 603);
        assert!(refused.events.is_empty());
    }
    Ok(())
}

#[test]
fn qualified_numbers_keep_the_registered_contact_and_flow() -> Result<(), SipError> {
    let config = configuration(Protocol::Tcp, false)?;
    let mut engine = Engine::new(config.clone(), [47; 32]).with_transport(Protocol::Tcp);
    let flow = StreamFlowId::new(31)?;
    let (registered, _) = admitted(
        &mut engine,
        Protocol::Tcp,
        Method::Register,
        "dialed-user",
        flow,
        0,
    )?;
    assert_eq!(status(&registered)?, 200);
    let mut call = outbound("carrier")?;
    call.endpoint = "+15550100@phone".into();
    let profile = OutboundProfile::capture(&config, &call.endpoint)?;
    let mut changed = outbound("changed")?;
    changed.endpoint = "5550123@phone".into();
    let prepared = engine.prepare_outbound(profile.clone(), 2)?;
    assert!(engine
        .originate_prepared_on_flow(changed, prepared, None, None, flow, 2)
        .is_err());
    let prepared = engine.prepare_outbound(profile, 2)?;
    let output = engine.originate_prepared_on_flow(call, prepared, None, None, flow, 2)?;
    routed(&output, flow);
    assert_eq!(
        request(&first(&output)?.bytes)?.uri.to_string(),
        rsipstack::sip::Uri::try_from(REGISTERED.replace("registered@", "+15550100@"))?.to_string()
    );
    for invalid in [
        "@phone",
        "number@",
        "name@phone@elsewhere",
        "bad user@phone",
    ] {
        assert!(OutboundProfile::capture(&config, invalid).is_err());
    }
    Ok(())
}

#[test]
fn publication_keeps_registered_preparations_outgoing_credentials_and_retained_fork_acks(
) -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let starting = configuration(protocol, false)?;
        let mut engine = Engine::new(starting.clone(), [47; 32]).with_transport(protocol);
        let flow = StreamFlowId::new(31)?;
        let (registered, _) = admitted(
            &mut engine,
            protocol,
            Method::Register,
            "registered-settings",
            flow,
            0,
        )?;
        assert_eq!(status(&registered)?, 200);
        let profile = OutboundProfile::capture(&starting, "phone")?;
        let prepared = engine.prepare_outbound(profile.clone(), 2)?;
        assert_eq!(prepared.flow(), Some(flow));
        let running = engine.prepare_outbound(profile, 2)?;
        let offered = engine.originate_prepared_on_flow(
            outbound("saved-winner")?,
            running,
            None,
            None,
            flow,
            2,
        )?;
        let invite = first(&offered)?.bytes.clone();
        let extra = format!("Contact: <{REGISTERED}>\r\nContent-Type: application/sdp\r\n");
        let selected = response(&invite, 200, "winner", &extra, SDP)?;
        engine.receive_on_flow(&selected, PEER, flow, 3)?;
        let fork = response(&invite, 200, "unselected", &extra, SDP)?;
        let cleanup = engine.receive_on_flow(&fork, PEER, flow, 4)?;
        routed(&cleanup, flow);
        let [ack, bye] = cleanup.datagrams.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(request(&ack.bytes)?.method, Method::Ack);
        assert_eq!(request(&bye.bytes)?.method, Method::Bye);
        let original_ack = ack.bytes.clone();
        engine.publish_configuration(PreparedEngineConfiguration::prepare(configuration(
            protocol, true,
        )?)?);
        let replayed = engine.receive_on_flow(&fork, PEER, flow, 5)?;
        routed(&replayed, flow);
        assert_eq!(replayed.datagrams.len(), 1);
        assert!(
            first(&replayed)?.bytes == original_ack,
            "Publication replaced a retained fork ACK"
        );
        assert!(engine.active_session("saved-winner", 7).is_some());
        let offered = engine.originate_prepared_on_flow(
            outbound("saved-preparation")?,
            prepared,
            None,
            None,
            flow,
            6,
        )?;
        routed(&offered, flow);
        let initial = request(&first(&offered)?.bytes)?;
        assert_eq!(
            initial.uri.to_string(),
            rsipstack::sip::Uri::try_from(REGISTERED)?.to_string()
        );
        assert!(std::str::from_utf8(&initial.body)?.contains("RTP/AVP 0 8"));
        assert_eq!(
            initial.from_header()?.typed()?.uri.user(),
            Some("shown-caller")
        );
        let challenge = response(&first(&offered)?.bytes, 401, "remote", "WWW-Authenticate: Digest realm=\"provider.test\", nonce=\"remote-nonce\", algorithm=MD5, qop=\"auth\"\r\n", "")?;
        let retry = engine.receive_on_flow(&challenge, PEER, flow, 7)?;
        routed(&retry, flow);
        let [ack, invited] = retry.datagrams.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(request(&ack.bytes)?.method, Method::Ack);
        let authenticated = request(&invited.bytes)?;
        let header = authenticated
            .authorization_header()
            .ok_or(SipError::InvalidPacket)?
            .value();
        assert_eq!(attribute(header, "username")?, "starting-remote");
        assert_eq!(attribute(header, "realm")?, "provider.test");
        assert_eq!(attribute(header, "uri")?, authenticated.uri.to_string());
        assert_eq!(attribute(header, "nc")?, "00000001");
        let qop = AuthQop::Auth {
            cnonce: attribute(header, "cnonce")?.into(),
            nc: 1,
        };
        let digest = DigestGenerator {
            username: "starting-remote",
            password: "starting-outbound",
            nonce: "remote-nonce",
            uri: &authenticated.uri,
            realm: "provider.test",
            method: &Method::Invite,
            qop: Some(&qop),
            algorithm: Algorithm::Md5,
        }
        .compute();
        assert_eq!(attribute(header, "response")?, digest);
        assert!(engine.active_session("saved-winner", 7).is_some());
    }
    Ok(())
}
