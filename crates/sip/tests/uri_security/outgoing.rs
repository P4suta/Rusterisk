use super::configuration;
use rsipstack::sip::{HeadersExt, Method, Request, SipMessage, Uri};
use rusterisk_config::{Codec, Protocol};
use rusterisk_sip::{Engine, Outbound, Output, Session, SipError, SipEvent};
use std::net::SocketAddr;

struct Fixture {
    engine: Engine,
    invite: Request,
    peer: SocketAddr,
}

impl Fixture {
    fn new(protocol: Protocol, scheme: &str) -> Result<Self, SipError> {
        let mut config = configuration(protocol, &format!("{scheme}:phone@127.0.0.2:5070"))?;
        config
            .endpoints
            .first_mut()
            .ok_or(SipError::InvalidPacket)?
            .outbound_auth
            .push("auth".into());
        let mut engine = Engine::new(config, [7; 32]).with_transport(protocol);
        let output = engine.originate(outbound("outbound-target")?, 0)?;
        let packet = output.datagrams.first().ok_or(SipError::InvalidPacket)?;
        let SipMessage::Request(invite) = SipMessage::try_from(packet.bytes.as_slice())? else {
            return Err(SipError::InvalidPacket);
        };
        Ok(Self {
            engine,
            invite,
            peer: packet.peer,
        })
    }

    fn answer(&self) -> Result<String, SipError> {
        Session::parse(std::str::from_utf8(&self.invite.body)?, &[Codec::Pcmu])?
            .answer("127.0.0.2:10002".parse()?, 9)
    }

    fn receive(&mut self, code: u16, headers: &str, body: &str) -> Result<Output, SipError> {
        self.engine.receive(
            reply(&self.invite, code, headers, body)?.as_bytes(),
            self.peer,
            1,
        )
    }
}

fn outbound(id: &str) -> Result<Outbound, SipError> {
    Ok(Outbound {
        id: id.into(),
        generation: 7,
        endpoint: "phone".into(),
        caller: "100".into(),
        local_rtp: "127.0.0.1:10000".parse()?,
    })
}

fn reply(invite: &Request, code: u16, headers: &str, body: &str) -> Result<String, SipError> {
    Ok(format!("SIP/2.0 {code} Response\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=peer\r\nCall-ID: {}\r\nCSeq: {}\r\n{headers}Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", invite.top_via_header()?.value(), invite.from_header()?.value(), invite.to_header()?.value(), invite.call_id_header()?.value(), invite.cseq_header()?.value(), body.len()))
}

fn requests(output: &Output) -> Result<Vec<Request>, SipError> {
    output
        .datagrams
        .iter()
        .map(
            |packet| match SipMessage::try_from(packet.bytes.as_slice())? {
                SipMessage::Request(request) => Ok(request),
                SipMessage::Response(_) => Err(SipError::InvalidPacket),
            },
        )
        .collect()
}

fn ended(output: &Output) -> bool {
    matches!(
        output.events.as_slice(),
        [SipEvent::Ended { id, generation: 7, cause: 21 }] if id == "outbound-target"
    )
}

fn contact(protocol: Protocol, scheme: &str) -> String {
    let parameter = match protocol {
        Protocol::Udp => "udp",
        Protocol::Tcp | Protocol::Tls => "tcp",
        Protocol::Ws | Protocol::Wss => "ws",
    };
    format!("Contact: <{scheme}:peer@127.0.0.2:5070;transport={parameter}>\r\n")
}

#[test]
fn valid_targets_still_receive_ack_and_bye_for_unacceptable_media() -> Result<(), SipError> {
    for protocol in [
        Protocol::Udp,
        Protocol::Tcp,
        Protocol::Tls,
        Protocol::Ws,
        Protocol::Wss,
    ] {
        let mut fixture = Fixture::new(protocol, "sip")?;
        let output = fixture.receive(200, &contact(protocol, "sip"), "invalid SDP")?;
        let requests = requests(&output)?;
        let [ack, bye] = requests.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(ack.method, Method::Ack);
        assert_eq!(bye.method, Method::Bye);
        assert!(ended(&output));
    }
    Ok(())
}

#[test]
fn response_contact_projection_ignores_disallowed_components() -> Result<(), SipError> {
    for protocol in [
        Protocol::Udp,
        Protocol::Tcp,
        Protocol::Tls,
        Protocol::Ws,
        Protocol::Wss,
    ] {
        for parameter in ["ttl", "TtL", "%74tl"] {
            let mut fixture = Fixture::new(protocol, "sip")?;
            let answer = fixture.answer()?;
            let base = contact(protocol, "sip");
            let base = base
                .strip_prefix("Contact: <")
                .and_then(|contact| contact.strip_suffix(">\r\n"))
                .ok_or(SipError::InvalidPacket)?;
            let expected = Uri::try_from(format!("{base};lr;x-route=one"))?;
            let headers = format!("Contact: <{base};{parameter}=1;method=OPTIONS;lr;x-route=one?Subject=ignored&body=ignored>\r\n");
            let output = fixture.receive(200, &headers, &answer)?;
            assert!(matches!(
                output.events.as_slice(),
                [SipEvent::Answered { .. }]
            ));
            let packets = requests(&output)?;
            let [ack] = packets.as_slice() else {
                return Err(SipError::InvalidPacket);
            };
            assert_eq!(ack.method, Method::Ack);
            assert_eq!(ack.uri, expected);
            assert!(ack.body.is_empty());
            assert!(!ack
                .headers
                .iter()
                .any(|header| header.name().eq_ignore_ascii_case("Subject")));
            let cancelled = fixture.engine.cancel("outbound-target", 7, 2)?;
            let packets = requests(&cancelled)?;
            let [bye] = packets.as_slice() else {
                return Err(SipError::InvalidPacket);
            };
            assert_eq!(bye.method, Method::Bye);
            assert_eq!(bye.uri, expected);
        }
    }
    Ok(())
}

#[test]
fn completed_response_retention_expires_without_replacing_the_dialog_target() -> Result<(), SipError>
{
    for tick in [false, true] {
        let mut fixture = Fixture::new(Protocol::Tls, "sip")?;
        let answer = fixture.answer()?;
        let original = reply(
            &fixture.invite,
            200,
            &contact(Protocol::Tls, "sip"),
            &answer,
        )?;
        let accepted = fixture
            .engine
            .receive(original.as_bytes(), fixture.peer, 1)?;
        let packets = requests(&accepted)?;
        let [original_ack] = packets.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        let repeated = fixture
            .engine
            .receive(original.as_bytes(), fixture.peer, 32_000)?;
        assert!(repeated.events.is_empty());
        let [repeated_ack] = repeated.datagrams.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(repeated_ack.bytes, accepted.datagrams[0].bytes);
        if tick {
            fixture.engine.tick(32_001)?;
        }
        let changed = reply(
            &fixture.invite,
            200,
            "Contact: <sip:changed@127.0.0.2:5070;transport=tls>\r\n",
            &answer,
        )?;
        let expired = fixture
            .engine
            .receive(changed.as_bytes(), fixture.peer, 32_001)?;
        assert!(expired.datagrams.is_empty());
        assert!(expired.events.is_empty());
        assert!(fixture
            .engine
            .active_session("outbound-target", 7)
            .is_some());
        let cancelled = fixture.engine.cancel("outbound-target", 7, 32_002)?;
        let packets = requests(&cancelled)?;
        let [bye] = packets.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(bye.method, Method::Bye);
        assert_eq!(bye.uri, original_ack.uri);
    }
    Ok(())
}

#[test]
fn invalid_targets_remain_terminal_during_both_cancellation_phases() -> Result<(), SipError> {
    for provisional in [false, true] {
        let mut fixture = Fixture::new(Protocol::Tls, "sip")?;
        let answer = fixture.answer()?;
        if provisional {
            fixture.receive(180, "", "")?;
        }
        fixture.engine.cancel("outbound-target", 7, 2)?;
        let invalid = reply(
            &fixture.invite,
            200,
            "Contact: <sip:peer@127.0.0.2:5070;transport=tls>\r\n",
            &answer,
        )?;
        let output = fixture
            .engine
            .receive(invalid.as_bytes(), fixture.peer, 3)?;
        assert!(ended(&output));
        assert!(output.datagrams.is_empty());
        let retried = fixture
            .engine
            .receive(invalid.as_bytes(), fixture.peer, 4)?;
        assert!(retried.events.is_empty());
        assert!(retried.datagrams.is_empty());
        let valid = reply(
            &fixture.invite,
            200,
            &contact(Protocol::Tls, "sip"),
            &answer,
        )?;
        let corrected = fixture.engine.receive(valid.as_bytes(), fixture.peer, 5)?;
        assert!(corrected.events.is_empty());
        assert!(corrected.datagrams.is_empty());
        assert!(fixture
            .engine
            .active_session("outbound-target", 7)
            .is_none());
    }
    Ok(())
}

#[test]
fn rejected_target_capacity_recovers_at_the_retention_deadline() -> Result<(), SipError> {
    let mut fixture = Fixture::new(Protocol::Udp, "sip")?;
    let answer = fixture.answer()?;
    let repeated = reply(
        &fixture.invite,
        200,
        "Contact: <sip:peer@127.0.0.2:5070;transport=tls>\r\n",
        &answer,
    )?;
    for index in 0..64 {
        let invite = if index == 0 {
            fixture.invite.clone()
        } else {
            let output = fixture
                .engine
                .originate(outbound(&format!("target-{index}"))?, 1)?;
            let packet = output.datagrams.first().ok_or(SipError::InvalidPacket)?;
            let SipMessage::Request(request) = SipMessage::try_from(packet.bytes.as_slice())?
            else {
                return Err(SipError::InvalidPacket);
            };
            request
        };
        let wire = reply(
            &invite,
            200,
            "Contact: <sip:peer@127.0.0.2:5070;transport=tls>\r\n",
            &answer,
        )?;
        let output = fixture.engine.receive(wire.as_bytes(), fixture.peer, 1)?;
        assert!(matches!(
            output.events.as_slice(),
            [SipEvent::Ended { cause: 21, .. }]
        ));
        assert!(output.datagrams.is_empty());
    }
    let retried = fixture
        .engine
        .receive(repeated.as_bytes(), fixture.peer, 32_000)?;
    assert!(retried.events.is_empty());
    assert!(retried.datagrams.is_empty());
    assert!(matches!(
        fixture.engine.originate(outbound("next")?, 32_000),
        Err(SipError::Capacity)
    ));
    fixture.engine.tick(32_001)?;
    let next = fixture.engine.originate(outbound("next")?, 32_002)?;
    assert_eq!(next.datagrams.len(), 1);
    assert!(next.events.is_empty());
    Ok(())
}

#[test]
fn repeated_success_cannot_change_the_target_of_an_established_dialog() -> Result<(), SipError> {
    let mut fixture = Fixture::new(Protocol::Tls, "sip")?;
    let answer = fixture.answer()?;
    let accepted = fixture.receive(200, &contact(Protocol::Tls, "sip"), &answer)?;
    let accepted = requests(&accepted)?;
    let [ack] = accepted.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    let repeated = fixture.receive(
        200,
        "Contact: <sip:changed@127.0.0.2:5070;transport=tls>\r\n",
        &answer,
    )?;
    assert!(repeated.events.is_empty());
    let repeated = requests(&repeated)?;
    let [repeated] = repeated.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(repeated.uri, ack.uri);
    let cancelled = fixture.engine.cancel("outbound-target", 7, 2)?;
    let packets = requests(&cancelled)?;
    let [bye] = packets.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(bye.method, Method::Bye);
    assert_eq!(bye.uri, ack.uri);
    Ok(())
}

#[test]
fn original_success_retries_preserve_the_ack_route_and_refreshed_dialog_target(
) -> Result<(), SipError> {
    let mut fixture = Fixture::new(Protocol::Tls, "sip")?;
    let body = "v=0\r\no=peer 9 1 IN IP4 127.0.0.2\r\ns=call\r\nc=IN IP4 127.0.0.2\r\nt=0 0\r\nm=audio 10002 RTP/AVP 0\r\na=sendrecv\r\n";
    let original = reply(&fixture.invite, 200, &contact(Protocol::Tls, "sip"), body)?;
    let first = fixture
        .engine
        .receive(original.as_bytes(), fixture.peer, 1)?;
    let first = requests(&first)?;
    let [original_ack] = first.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    let local = fixture
        .invite
        .contact_header()?
        .value()
        .trim_matches(['<', '>']);
    let from = format!("{};tag=peer", fixture.invite.to_header()?.value());
    let to = fixture.invite.from_header()?.value();
    let call_id = fixture.invite.call_id_header()?.value();
    let updated = body.replace("peer 9 1", "peer 9 2");
    let refreshed = "sip:refreshed@127.0.0.2:5070;transport=tcp";
    let invite = format!("INVITE {local} SIP/2.0\r\nVia: SIP/2.0/TLS {};branch=z9hG4bK-target-refresh;rport\r\nFrom: {from}\r\nTo: {to}\r\nCall-ID: {call_id}\r\nCSeq: 10 INVITE\r\nContact: <{refreshed}>\r\nMax-Forwards: 70\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{updated}", fixture.peer, updated.len());
    let accepted = fixture.engine.receive(invite.as_bytes(), fixture.peer, 2)?;
    let packet = accepted.datagrams.first().ok_or(SipError::InvalidPacket)?;
    let SipMessage::Response(response) = SipMessage::try_from(packet.bytes.as_slice())? else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(response.status_code.code(), 200);
    let repeated = fixture
        .engine
        .receive(original.as_bytes(), fixture.peer, 3)?;
    assert!(repeated.events.is_empty());
    let repeated = requests(&repeated)?;
    let [repeated] = repeated.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(repeated.uri, original_ack.uri);
    let ack = format!("ACK {local} SIP/2.0\r\nVia: SIP/2.0/TLS {};branch=z9hG4bK-target-refresh-ack;rport\r\nFrom: {from}\r\nTo: {to}\r\nCall-ID: {call_id}\r\nCSeq: 10 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", fixture.peer);
    fixture.engine.receive(ack.as_bytes(), fixture.peer, 4)?;
    let output = fixture.engine.cancel("outbound-target", 7, 5)?;
    let packets = requests(&output)?;
    let [bye] = packets.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(bye.method, Method::Bye);
    assert_eq!(bye.uri, Uri::try_from(refreshed)?);
    Ok(())
}

#[test]
fn unusable_success_targets_end_without_generating_deprecated_requests() -> Result<(), SipError> {
    for protocol in [Protocol::Tls, Protocol::Wss] {
        let mut fixture = Fixture::new(protocol, "sip")?;
        let answer = fixture.answer()?;
        let output = fixture.receive(
            200,
            "Contact: <sip:peer@127.0.0.2:5070;transport=tls>\r\n",
            &answer,
        )?;
        assert!(ended(&output));
        assert!(output.datagrams.is_empty());
        assert!(fixture
            .engine
            .active_session("outbound-target", 7)
            .is_none());
        assert!(fixture.engine.tick(600)?.datagrams.is_empty());
    }
    Ok(())
}

#[test]
fn successful_responses_cannot_downgrade_a_sips_dialog_target() -> Result<(), SipError> {
    for protocol in [Protocol::Tls, Protocol::Wss] {
        let mut fixture = Fixture::new(protocol, "sips")?;
        let answer = fixture.answer()?;
        let output = fixture.receive(200, &contact(protocol, "sip"), &answer)?;
        assert!(ended(&output));
        assert!(output.datagrams.is_empty());
        assert!(fixture
            .engine
            .active_session("outbound-target", 7)
            .is_none());
    }
    Ok(())
}

#[test]
fn success_requires_one_usable_contact_before_media_publication() -> Result<(), SipError> {
    for headers in [
        "",
        "Contact: *\r\n",
        "Contact: <sip:first@127.0.0.2:5070>, <sip:second@127.0.0.2:5070>\r\n",
        "Contact: <sip:first@127.0.0.2:5070>\r\nContact: <sip:second@127.0.0.2:5070>\r\n",
    ] {
        let mut fixture = Fixture::new(Protocol::Udp, "sip")?;
        let answer = fixture.answer()?;
        let output = fixture.receive(200, headers, &answer)?;
        assert!(ended(&output), "{headers}");
        assert!(output.datagrams.is_empty());
        assert!(fixture
            .engine
            .active_session("outbound-target", 7)
            .is_none());
    }
    Ok(())
}

#[test]
fn challenge_contacts_do_not_replace_the_invite_acknowledgment_target() -> Result<(), SipError> {
    let mut fixture = Fixture::new(Protocol::Udp, "sip")?;
    let output = fixture.receive(401, "Contact: \"Peer name > other\" <sip:peer@127.0.0.2:5070>\r\nWWW-Authenticate: Digest realm=\"rusterisk\", nonce=\"nonce\", algorithm=MD5, qop=\"auth\"\r\n", "")?;
    let requests = requests(&output)?;
    let [ack, retry] = requests.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(ack.method, Method::Ack);
    assert_eq!(ack.uri, fixture.invite.uri);
    assert_eq!(retry.method, Method::Invite);
    assert_eq!(retry.uri, fixture.invite.uri);
    assert!(output.events.is_empty());
    Ok(())
}

#[test]
fn valid_targets_keep_required_acknowledgments_and_cancelled_answer_cleanup() -> Result<(), SipError>
{
    for protocol in [
        Protocol::Udp,
        Protocol::Tcp,
        Protocol::Tls,
        Protocol::Ws,
        Protocol::Wss,
    ] {
        for cancelled in [false, true] {
            let mut fixture = Fixture::new(protocol, "sip")?;
            let answer = fixture.answer()?;
            if cancelled {
                fixture.engine.cancel("outbound-target", 7, 0)?;
            }
            let output = fixture.receive(200, &contact(protocol, "sip"), &answer)?;
            let packets = requests(&output)?;
            assert_eq!(
                packets
                    .iter()
                    .map(|packet| packet.method.clone())
                    .collect::<Vec<_>>(),
                if cancelled {
                    [Method::Ack, Method::Bye].as_slice()
                } else {
                    [Method::Ack].as_slice()
                }
            );
            if cancelled {
                assert!(fixture
                    .engine
                    .active_session("outbound-target", 7)
                    .is_none());
            } else {
                assert!(matches!(
                    output.events.as_slice(),
                    [SipEvent::Answered { .. }]
                ));
            }
        }
    }
    Ok(())
}
