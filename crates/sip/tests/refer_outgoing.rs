use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method, Request};
use rusterisk_config::{Config, Protocol};
use rusterisk_sip::{DialogRefer, Engine, Outbound, Output, ReferProgress, SipError, SipEvent};
use std::net::SocketAddr;
#[path = "refer_dialog/support.rs"]
mod support;
use support::{notifies, receipt, request, status};

struct Outgoing {
    engine: Engine,
    peer: SocketAddr,
    invite: Request,
    remote_to: String,
    transport: &'static str,
}

impl Outgoing {
    fn new(protocol: Protocol, allow_transfer: bool) -> Result<Self, SipError> {
        let (name, transport) = match protocol {
            Protocol::Udp => ("udp", "UDP"),
            Protocol::Tcp => ("tcp", "TCP"),
            Protocol::Tls | Protocol::Ws | Protocol::Wss => return Err(SipError::Unsupported),
        };
        let config = Config::parse(
            "[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test\n",
            &format!("[signaling]\ntype=transport\nprotocol={name}\nbind=127.0.0.1:5060\n[credential]\ntype=auth\nusername=local-phone\npassword=password\nrealm=pbx\n[configured]\ntype=aor\ncontact=sip:bob@127.0.0.1:5062\n[bob]\ntype=endpoint\ntransport=signaling\nauth=credential\noutbound_auth=credential\naors=configured\ncontext=callee\nallow=ulaw\nallow_transfer={}\n", if allow_transfer { "yes" } else { "no" }), "", "",
        )?;
        let mut engine = Engine::new(config, [19; 32]).with_transport(protocol);
        let offered = engine.originate(
            Outbound {
                id: "outgoing-refer".into(),
                generation: 19,
                endpoint: "bob".into(),
                caller: "display-only".into(),
                local_rtp: "127.0.0.1:10000".parse()?,
                timeout_ms: Some(5000),
            },
            0,
        )?;
        let invite = request(
            &offered
                .datagrams
                .first()
                .ok_or(SipError::InvalidPacket)?
                .bytes,
        )?;
        let peer = "127.0.0.1:5062".parse()?;
        let challenge = format!("SIP/2.0 401 Unauthorized\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=challenge\r\nCall-ID: {}\r\nCSeq: {}\r\nWWW-Authenticate: Digest realm=\"pbx\", nonce=\"outgoing-refer\", algorithm=MD5, qop=\"auth\"\r\nContent-Length: 0\r\n\r\n", invite.top_via_header()?.value(), invite.from_header()?.value(), invite.to_header()?.value(), invite.call_id_header()?.value(), invite.cseq_header()?.value());
        let retried = engine.receive(challenge.as_bytes(), peer, 0)?;
        assert_eq!(retried.datagrams.len(), 2);
        assert_eq!(request(&retried.datagrams[0].bytes)?.method, Method::Ack);
        let invite = request(&retried.datagrams[1].bytes)?;
        assert_eq!(invite.method, Method::Invite);
        assert!(invite
            .authorization_header()
            .ok_or(SipError::InvalidPacket)?
            .value()
            .contains("username=\"local-phone\""));
        let remote_to = format!("{};tag=callee", invite.to_header()?.value());
        let body = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=call\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 10002 RTP/AVP 0 101\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n";
        let answer = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {remote_to}\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:desk@127.0.0.1:5062>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", invite.top_via_header()?.value(), invite.from_header()?.value(), invite.call_id_header()?.value(), invite.cseq_header()?.value(), body.len());
        let answered = engine.receive(answer.as_bytes(), peer, 1)?;
        assert!(answered.events.iter().any(|event| matches!(event, SipEvent::Answered { id, generation: 19, .. } if id == "outgoing-refer")));
        let ack = request(
            &answered
                .datagrams
                .first()
                .ok_or(SipError::InvalidPacket)?
                .bytes,
        )?;
        assert_eq!(ack.method, Method::Ack);
        Ok(Self {
            engine,
            peer,
            invite,
            remote_to,
            transport,
        })
    }

    fn packet(&self, method: &str, sequence: u32, extra: &str) -> Result<Vec<u8>, SipError> {
        let target = self.invite.contact_header()?.typed()?.uri;
        Ok(format!("{method} {target} SIP/2.0\r\nVia: SIP/2.0/{} 127.0.0.1:5062;branch=z9hG4bK-{method}-{sequence};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} {method}\r\nContact: <sip:desk@127.0.0.1:5062>\r\nMax-Forwards: 70\r\n{extra}Content-Length: 0\r\n\r\n", self.transport, self.remote_to, self.invite.from_header()?.value(), self.invite.call_id_header()?.value()).into_bytes())
    }

    fn receive(
        &mut self,
        method: &str,
        sequence: u32,
        extra: &str,
        now: u64,
    ) -> Result<Output, SipError> {
        self.engine
            .receive(&self.packet(method, sequence, extra)?, self.peer, now)
    }

    fn refer(&mut self) -> Result<DialogRefer, SipError> {
        self.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?
            .events
            .into_iter()
            .find_map(|event| {
                if let SipEvent::BlindTransferRequested {
                    id,
                    generation: 19,
                    refer,
                } = event
                {
                    assert_eq!(id, "outgoing-refer");
                    Some(refer)
                } else {
                    None
                }
            })
            .ok_or(SipError::InvalidPacket)
    }
}

#[test]
fn outgoing_refer_uses_the_established_sequence_and_sends_after_local_bye() -> Result<(), SipError>
{
    for protocol in [Protocol::Udp, Protocol::Tcp] {
        let mut fixture = Outgoing::new(protocol, true)?;
        let refer = fixture.refer()?;
        let accepted = fixture
            .engine
            .refer_progress(refer.key(), ReferProgress::Trying, 6)?;
        assert!(status(&accepted, 202));
        let initial = notifies(&accepted)?.pop().ok_or(SipError::InvalidPacket)?;
        assert_eq!(initial.uri.to_string(), "sip:desk@127.0.0.1:5062");
        assert_eq!(
            initial.from_header()?.value(),
            fixture.invite.from_header()?.value()
        );
        assert_eq!(initial.to_header()?.value(), fixture.remote_to);
        fixture
            .engine
            .receive(&receipt(&initial, 200)?, fixture.peer, 7)?;
        let digit = rusterisk_core::DtmfDigit::from_event(1).ok_or(SipError::InvalidPacket)?;
        let sent = fixture
            .engine
            .send_info("outgoing-refer", 19, digit, 100, 8)?;
        let info = request(&sent.datagrams.first().ok_or(SipError::InvalidPacket)?.bytes)?;
        assert!(info.cseq_header()?.typed()?.seq > initial.cseq_header()?.typed()?.seq);
        fixture
            .engine
            .receive(&receipt(&info, 200)?, fixture.peer, 9)?;
        let cancelled = fixture.engine.cancel("outgoing-refer", 19, 10)?;
        let bye = request(
            &cancelled
                .datagrams
                .first()
                .ok_or(SipError::InvalidPacket)?
                .bytes,
        )?;
        assert_eq!(bye.method, Method::Bye);
        assert!(bye.cseq_header()?.typed()?.seq > info.cseq_header()?.typed()?.seq);
        fixture
            .engine
            .receive(&receipt(&bye, 200)?, fixture.peer, 11)?;
        assert!(fixture.engine.tick(40_000)?.datagrams.is_empty());
        let completed =
            fixture
                .engine
                .refer_progress(refer.key(), ReferProgress::Answered, 40_001)?;
        let notification = notifies(&completed)?.pop().ok_or(SipError::InvalidPacket)?;
        assert_eq!(notification.body, b"SIP/2.0 200 OK\r\n");
        assert!(notification.cseq_header()?.typed()?.seq > bye.cseq_header()?.typed()?.seq);
        let settled =
            fixture
                .engine
                .receive(&receipt(&notification, 200)?, fixture.peer, 40_002)?;
        assert!(settled.events.iter().any(|event| matches!(event, SipEvent::ReferSubscriptionEnded { refer: key } if *key == refer.key())));
        assert!(fixture.engine.tick(80_000)?.datagrams.is_empty());
    }
    Ok(())
}

#[test]
fn configured_peer_authority_does_not_claim_the_displayed_or_outbound_authenticated_identity(
) -> Result<(), SipError> {
    let mut outgoing = Outgoing::new(Protocol::Udp, true)?;
    let refer = outgoing.refer()?;
    assert_eq!(refer.authority().endpoint(), "bob");
    assert_eq!(refer.authority().context(), "callee");
    assert_eq!(refer.authority().authenticated_identity(), None);
    let mut incoming = support::Fixture::new(true, true)?;
    let requested = incoming.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?;
    assert!(requested.events.iter().any(
        |event| matches!(event, SipEvent::BlindTransferRequested { id, .. } if id == &incoming.id)
    ));
    let authenticated = support::refer(requested)?;
    assert_eq!(
        authenticated.authority().authenticated_identity(),
        Some("phone")
    );
    assert_eq!(authenticated.authority().endpoint(), "alice");
    assert_eq!(authenticated.authority().context(), "internal");
    Ok(())
}

#[test]
fn outgoing_refer_requires_the_current_dialog_and_honors_transfer_denial() -> Result<(), SipError> {
    let mut fixture = Outgoing::new(Protocol::Udp, true)?;
    let packet = fixture.packet("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n")?;
    let changed = std::str::from_utf8(&packet)?.replace("tag=callee", "tag=unrelated");
    let rejected = fixture
        .engine
        .receive(changed.as_bytes(), fixture.peer, 5)?;
    assert!(status(&rejected, 481));
    assert!(rejected.events.is_empty());
    let rejected = fixture
        .engine
        .receive(&packet, "127.0.0.1:5063".parse()?, 6)?;
    assert!(status(&rejected, 481));
    assert!(rejected.events.is_empty());
    let requested = fixture.engine.receive(&packet, fixture.peer, 7)?;
    assert!(requested.events.iter().any(|event| matches!(
        event,
        SipEvent::BlindTransferRequested { generation: 19, .. }
    )));
    let mut denied = Outgoing::new(Protocol::Udp, false)?;
    let rejected = denied.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?;
    assert!(status(&rejected, 603));
    assert!(rejected.events.is_empty());
    assert!(status(&denied.receive("BYE", 4, "", 6)?, 200));
    Ok(())
}
