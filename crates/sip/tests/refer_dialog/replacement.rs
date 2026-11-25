use super::support::{request, status, Fixture};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Request};
use rusterisk_config::{Codec, Config, Protocol};
use rusterisk_sip::{Engine, Outbound, ReferFailure, Replaces, Session, SipError, SipEvent};
use std::net::SocketAddr;

fn incoming_selector(fixture: &Fixture) -> Result<Replaces, String> {
    let local = fixture
        .answer
        .to_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?;
    let remote = fixture
        .answer
        .from_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?;
    Replaces::parse(&format!(
        "{};to-tag={};from-tag={}",
        fixture.answer.call_id_header().map_err(failure)?.value(),
        local.tag().ok_or("Missing local tag")?.value(),
        remote.tag().ok_or("Missing remote tag")?.value(),
    ))
    .map_err(failure)
}

fn failure(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[test]
fn incoming_dialog_lookup_uses_local_to_remote_from_and_the_live_generation() -> Result<(), String>
{
    let fixture = Fixture::new(true, true).map_err(failure)?;
    let selector = incoming_selector(&fixture)?;
    let found = fixture
        .engine
        .replacement_dialog(&selector)
        .map_err(|error| format!("{error:?}"))?
        .ok_or("Missing consultation")?;
    assert_eq!(found.id(), fixture.id);
    assert_eq!(found.generation(), 7);
    assert_eq!(found.authority().endpoint(), "alice");
    assert_eq!(found.authority().authenticated_identity(), Some("phone"));
    for (call_id, local, remote) in [
        (
            selector.call_id().to_ascii_uppercase(),
            selector.to_tag().to_owned(),
            selector.from_tag().to_owned(),
        ),
        (
            selector.call_id().to_owned(),
            "different-local".into(),
            selector.from_tag().to_owned(),
        ),
        (
            selector.call_id().to_owned(),
            selector.to_tag().to_owned(),
            "Caller".into(),
        ),
        (
            selector.call_id().to_owned(),
            selector.from_tag().to_owned(),
            selector.to_tag().to_owned(),
        ),
    ] {
        let other = Replaces::parse(&format!("{call_id};to-tag={local};from-tag={remote}"))
            .map_err(failure)?;
        assert!(fixture
            .engine
            .replacement_dialog(&other)
            .map_err(|error| format!("{error:?}"))?
            .is_none());
    }
    let debug = format!("{found:?}");
    for private in [
        fixture.id.as_str(),
        selector.call_id(),
        selector.to_tag(),
        selector.from_tag(),
        "phone",
    ] {
        assert!(!debug.contains(private));
    }
    Ok(())
}

#[test]
fn confirmed_early_only_and_ended_dialogs_cannot_be_replaced() -> Result<(), String> {
    let mut fixture = Fixture::new(true, true).map_err(failure)?;
    let selector = incoming_selector(&fixture)?;
    let early = Replaces::parse(&format!(
        "{};to-tag={};from-tag={};early-only",
        selector.call_id(),
        selector.to_tag(),
        selector.from_tag()
    ))
    .map_err(failure)?;
    assert!(matches!(
        fixture.engine.replacement_dialog(&early),
        Err(ReferFailure::Busy)
    ));
    assert!(status(
        &fixture.receive("BYE", 3, "", 5).map_err(failure)?,
        200
    ));
    assert!(matches!(
        fixture.engine.replacement_dialog(&selector),
        Err(ReferFailure::Declined)
    ));
    fixture.engine.tick(32_005).map_err(failure)?;
    assert!(fixture
        .engine
        .replacement_dialog(&selector)
        .map_err(|error| format!("{error:?}"))?
        .is_none());
    Ok(())
}

struct Outgoing {
    engine: Engine,
    invite: Request,
    peer: SocketAddr,
    scheme: &'static str,
}

impl Outgoing {
    fn new(protocol: Protocol, scheme: &'static str) -> Result<Self, SipError> {
        let (transport, files) = match protocol {
            Protocol::Udp => ("udp", ""),
            Protocol::Tls => (
                "tls",
                "cert_file=certificate.pem\npriv_key_file=private.pem\n",
            ),
            Protocol::Tcp | Protocol::Ws | Protocol::Wss => return Err(SipError::Unsupported),
        };
        let config = Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=test\n",
            &format!("[native]\ntype=transport\nprotocol={transport}\nbind=127.0.0.1:5060\n{files}[auth]\ntype=auth\nusername=phone\npassword=password\n[target]\ntype=aor\ncontact={scheme}:phone@127.0.0.2:5070\n[phone]\ntype=endpoint\ntransport=native\nauth=auth\naors=target\nallow=ulaw\n"),
            "", "",
        )?;
        let mut engine = Engine::new(config, [11; 32]).with_transport(protocol);
        let output = engine.originate(
            Outbound {
                id: "consultation".into(),
                generation: 17,
                endpoint: "phone".into(),
                caller: "Displayed".into(),
                local_rtp: "127.0.0.1:10000".parse()?,
            },
            0,
        )?;
        let packet = output.datagrams.first().ok_or(SipError::InvalidPacket)?;
        Ok(Self {
            engine,
            invite: request(&packet.bytes)?,
            peer: packet.peer,
            scheme,
        })
    }

    fn selector(&self) -> Result<Replaces, String> {
        let from = self
            .invite
            .from_header()
            .map_err(failure)?
            .typed()
            .map_err(failure)?;
        Replaces::parse(&format!(
            "{};to-tag={};from-tag=RemoteCase",
            self.invite.call_id_header().map_err(failure)?.value(),
            from.tag().ok_or("Missing local tag")?.value()
        ))
        .map_err(failure)
    }

    fn answer(&mut self) -> Result<(), SipError> {
        let body = Session::parse(std::str::from_utf8(&self.invite.body)?, &[Codec::Pcmu])?
            .answer("127.0.0.2:10002".parse()?, 9)?;
        let response = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=RemoteCase\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <{}:phone@127.0.0.2:5070>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", self.invite.top_via_header()?.value(), self.invite.from_header()?.value(), self.invite.to_header()?.value(), self.invite.call_id_header()?.value(), self.invite.cseq_header()?.value(), self.scheme, body.len());
        let output = self.engine.receive(response.as_bytes(), self.peer, 1)?;
        assert!(matches!(
            output.events.as_slice(),
            [SipEvent::Answered { generation: 17, .. }]
        ));
        Ok(())
    }
}

#[test]
fn outgoing_dialog_lookup_reverses_the_original_invite_tags() -> Result<(), String> {
    let mut fixture = Outgoing::new(Protocol::Udp, "sip").map_err(failure)?;
    let selector = fixture.selector()?;
    assert!(fixture
        .engine
        .replacement_dialog(&selector)
        .map_err(|error| format!("{error:?}"))?
        .is_none());
    fixture.answer().map_err(failure)?;
    let found = fixture
        .engine
        .replacement_dialog(&selector)
        .map_err(|error| format!("{error:?}"))?
        .ok_or("Missing outgoing consultation")?;
    assert_eq!(found.id(), "consultation");
    assert_eq!(found.generation(), 17);
    assert_eq!(found.authority().authenticated_identity(), None);
    let reversed = Replaces::parse(&format!(
        "{};to-tag={};from-tag={}",
        selector.call_id(),
        selector.from_tag(),
        selector.to_tag()
    ))
    .map_err(failure)?;
    assert!(fixture
        .engine
        .replacement_dialog(&reversed)
        .map_err(|error| format!("{error:?}"))?
        .is_none());
    fixture
        .engine
        .cancel("consultation", 17, 2)
        .map_err(failure)?;
    assert!(matches!(
        fixture.engine.replacement_dialog(&selector),
        Err(ReferFailure::Declined)
    ));
    Ok(())
}

#[test]
fn sip_over_tls_remains_distinct_from_sips_replacement_authorization() -> Result<(), String> {
    for (scheme, supported) in [("sip", true), ("sips", false)] {
        let mut fixture = Outgoing::new(Protocol::Tls, scheme).map_err(failure)?;
        fixture.answer().map_err(failure)?;
        let found = fixture.engine.replacement_dialog(&fixture.selector()?);
        if supported {
            assert!(found.map_err(|error| format!("{error:?}"))?.is_some());
        } else {
            assert!(matches!(found, Err(ReferFailure::Unsupported)));
        }
    }
    Ok(())
}

#[test]
fn secure_transport_does_not_erase_the_source_dialogs_sips_authority_boundary() -> Result<(), String>
{
    for scheme in ["sip", "sips"] {
        let mut fixture = Outgoing::new(Protocol::Tls, scheme).map_err(failure)?;
        fixture.answer().map_err(failure)?;
        let target = fixture
            .invite
            .contact_header()
            .map_err(failure)?
            .typed()
            .map_err(failure)?
            .uri;
        let packet = format!(
            "REFER {target} SIP/2.0\r\nVia: SIP/2.0/TLS 127.0.0.2:5070;branch=z9hG4bK-attended;rport\r\nFrom: {};tag=RemoteCase\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 3 REFER\r\nContact: <{scheme}:phone@127.0.0.2:5070>\r\nMax-Forwards: 70\r\nRefer-To: <sip:target@localhost?Replaces=consultation%3Bto-tag%3DLocal%3Bfrom-tag%3DRemote>\r\nContent-Length: 0\r\n\r\n",
            fixture.invite.to_header().map_err(failure)?.value(),
            fixture.invite.from_header().map_err(failure)?.value(),
            fixture.invite.call_id_header().map_err(failure)?.value(),
        );
        let output = fixture
            .engine
            .receive(packet.as_bytes(), fixture.peer, 2)
            .map_err(failure)?;
        if scheme == "sips" {
            assert!(status(&output, 501));
            assert!(output.events.is_empty());
        } else {
            assert!(output.datagrams.is_empty());
            assert!(matches!(
                output.events.as_slice(),
                [SipEvent::AttendedTransferRequested { generation: 17, .. }]
            ));
        }
    }
    Ok(())
}
