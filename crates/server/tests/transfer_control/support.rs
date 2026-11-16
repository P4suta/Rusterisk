use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    HeadersExt, Method, Response, SipMessage, Uri,
};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_server::{
    CallKey, Control, ControlError, Effect, IncomingCall, IncomingTarget, Milestone, Signal,
};
use rusterisk_sip::{DialogRefer, Engine, SipEvent};
use std::net::SocketAddr;

pub(super) fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

pub(super) struct Pair {
    pub control: Control,
    pub source: CallKey,
    pub survivor: CallKey,
    pub bridge: u64,
    engine: Engine,
    answer: Response,
    peer: SocketAddr,
}

impl Pair {
    pub fn new() -> Result<Self, ControlError> {
        let config = Config::parse(
            "[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test\n",
            "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[alice-auth]\ntype=auth\nusername=phone\npassword=password\n[bob-auth]\ntype=auth\nusername=bob\npassword=bob-password\n[charlie-auth]\ntype=auth\nusername=charlie\npassword=charlie-password\n[alice-aor]\ntype=aor\n[bob-aor]\ntype=aor\ncontact=sip:bob@127.0.0.1:5062\n[charlie-aor]\ntype=aor\ncontact=sip:charlie@127.0.0.1:5063\n[alice]\ntype=endpoint\ntransport=udp\nauth=alice-auth\naors=alice-aor\ncontext=internal\nallow=ulaw\n[bob]\ntype=endpoint\ntransport=udp\nauth=bob-auth\naors=bob-aor\ncontext=internal\nallow=ulaw\n[charlie]\ntype=endpoint\ntransport=udp\nauth=charlie-auth\naors=charlie-aor\ncontext=internal\nallow=ulaw\n",
            "", "[internal]\nexten => 100,1,Goto(routed,100,1)\nexten => 200,1,Dial(PJSIP/charlie,5)\n[routed]\nexten => 100,1,Dial(PJSIP/bob,5)\nexten => 200,1,Hangup()\n",
        ).map_err(failure)?;
        let mut engine = Engine::new(config.clone(), [7; 32]);
        let peer: SocketAddr = "127.0.0.1:5061".parse().map_err(failure)?;
        let challenged = engine
            .receive(invite("", 1).as_bytes(), peer, 0)
            .map_err(failure)?;
        let challenge = match SipMessage::try_from(
            challenged
                .datagrams
                .first()
                .ok_or(ControlError::Invalid("Missing challenge"))?
                .bytes
                .as_slice(),
        )
        .map_err(failure)?
        {
            SipMessage::Response(response) => response,
            SipMessage::Request(_) => {
                return Err(ControlError::Invalid("Expected authentication challenge"))
            }
        };
        let challenge = WwwAuthenticate::parse(
            challenge
                .headers
                .iter()
                .find(|header| header.name().eq_ignore_ascii_case("WWW-Authenticate"))
                .ok_or(ControlError::Invalid("Missing challenge header"))?
                .value(),
        )
        .map_err(failure)?;
        let uri = Uri::try_from("sip:100@localhost").map_err(failure)?;
        let qop = AuthQop::Auth {
            cnonce: "transfer-client".into(),
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
        let authorization = format!("Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"sip:100@localhost\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"transfer-client\", nc=00000001\r\n", challenge.realm, challenge.nonce);
        let admitted = engine
            .receive(invite(&authorization, 2).as_bytes(), peer, 1)
            .map_err(failure)?;
        let incoming = admitted
            .events
            .into_iter()
            .next()
            .ok_or(ControlError::Invalid("Missing admitted caller"))?;
        let SipEvent::Incoming {
            id,
            endpoint,
            caller,
            context,
            extension,
            ..
        } = incoming
        else {
            return Err(ControlError::Invalid("Expected admitted caller"));
        };
        let mut control = Control::with_configuration(8, &config);
        control.subscribe("calls")?;
        let id = ChannelId::try_from(id)?;
        control.incoming(
            IncomingCall {
                id: id.clone(),
                endpoint,
                caller,
                context,
                extension,
                target: IncomingTarget::Dialplan,
            },
            "incoming",
        )?;
        let source = control.key(&id)?;
        let started = control.start_dialplan(&source, config.dialplan.clone(), 1, "dial")?;
        let [Effect::Start { key: survivor, .. }] = started.effects.as_slice() else {
            return Err(ControlError::Invalid("Expected one Dial branch"));
        };
        let survivor = survivor.clone();
        let answered = control.signal_at(
            Signal::Milestone {
                key: survivor.clone(),
                milestone: Milestone::Answered,
            },
            "peer answer",
            2,
        )?;
        assert!(answered
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Answer { key } if key == &source)));
        let answer = engine
            .answer(
                source.id.as_str(),
                source.generation,
                "127.0.0.1:10000".parse().map_err(failure)?,
                3,
            )
            .map_err(failure)?;
        let answer = match SipMessage::try_from(
            answer
                .datagrams
                .first()
                .ok_or(ControlError::Invalid("Missing caller answer"))?
                .bytes
                .as_slice(),
        )
        .map_err(failure)?
        {
            SipMessage::Response(response) => response,
            SipMessage::Request(_) => return Err(ControlError::Invalid("Expected caller answer")),
        };
        let ack = related(&answer, "ACK", 2, "").map_err(failure)?;
        engine.receive(&ack, peer, 4).map_err(failure)?;
        let joined = control.signal_at(
            Signal::Milestone {
                key: source.clone(),
                milestone: Milestone::Answered,
            },
            "caller answer",
            4,
        )?;
        let bridge = joined
            .effects
            .iter()
            .find_map(|effect| {
                if let Effect::DialBridge {
                    parent,
                    generation,
                    peer: Some(peer),
                } = effect
                {
                    assert_eq!(parent, &source);
                    assert_eq!(peer, &survivor);
                    Some(*generation)
                } else {
                    None
                }
            })
            .ok_or(ControlError::Invalid("Missing initial bridge"))?;
        Ok(Self {
            control,
            source,
            survivor,
            bridge,
            engine,
            answer,
            peer,
        })
    }

    pub fn request(&mut self, extension: &str) -> Result<DialogRefer, ControlError> {
        let packet = related(
            &self.answer,
            "REFER",
            3,
            &format!("Refer-To: <sip:{extension}@outside.invalid>\r\n"),
        )
        .map_err(failure)?;
        let requested = self
            .engine
            .receive(&packet, self.peer, 5)
            .map_err(failure)?;
        assert!(requested.datagrams.is_empty());
        let event = requested
            .events
            .into_iter()
            .next()
            .ok_or(ControlError::Invalid("Missing transfer request"))?;
        let SipEvent::BlindTransferRequested {
            id,
            generation,
            refer,
        } = event
        else {
            return Err(ControlError::Invalid("Expected transfer request"));
        };
        assert_eq!(id, self.source.id.as_str());
        assert_eq!(generation, self.source.generation);
        Ok(refer)
    }
}

fn invite(authorization: &str, sequence: u32) -> String {
    let body = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=call\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 10002 RTP/AVP 0\r\na=sendrecv\r\n";
    format!("INVITE sip:100@localhost SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-{sequence};rport\r\nFrom: Admin <sip:spoofed@localhost>;tag=caller\r\nTo: <sip:100@localhost>\r\nCall-ID: transfer-control\r\nCSeq: {sequence} INVITE\r\nContact: <sip:phone@127.0.0.1:5061>\r\nMax-Forwards: 70\r\n{authorization}Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", body.len())
}

fn related(
    answer: &Response,
    method: &str,
    sequence: u32,
    extra: &str,
) -> Result<Vec<u8>, rsipstack::sip::Error> {
    Ok(format!("{method} sip:100@localhost SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-{method}-{sequence};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} {method}\r\nContact: <sip:phone@127.0.0.1:5061>\r\nMax-Forwards: 70\r\n{extra}Content-Length: 0\r\n\r\n", answer.from_header()?.value(), answer.to_header()?.value(), answer.call_id_header()?.value()).into_bytes())
}
