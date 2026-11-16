use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    HeadersExt, Method, Request, Response, SipMessage, Uri,
};
use rusterisk_sip::{DialogRefer, Engine, Output, SipError, SipEvent};
use std::net::SocketAddr;

pub(super) struct Fixture {
    pub engine: Engine,
    pub peer: SocketAddr,
    pub id: String,
    pub answer: Response,
}

impl Fixture {
    pub fn new(allow_transfer: bool, refer_blind_progress: bool) -> Result<Self, SipError> {
        let config = rusterisk_config::Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=test\n",
            &format!("[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[credential]\ntype=auth\nusername=phone\npassword=password\n[aor]\ntype=aor\n[alice]\ntype=endpoint\ntransport=udp\nauth=credential\naors=aor\ncontext=internal\nallow=ulaw\nallow_transfer={}\nrefer_blind_progress={}\n", if allow_transfer { "yes" } else { "no" }, if refer_blind_progress { "yes" } else { "no" }),
            "", "",
        )?;
        let mut engine = Engine::new(config, [7; 32]);
        let peer = "127.0.0.1:5061".parse()?;
        let body = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=call\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 10002 RTP/AVP 0 101\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n";
        let invite = |authorization: &str, sequence: u32| {
            format!("INVITE sip:100@localhost SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-{sequence};rport\r\nFrom: Admin <sip:spoofed@localhost>;tag=caller\r\nTo: <sip:100@localhost>\r\nCall-ID: refer-dialog\r\nCSeq: {sequence} INVITE\r\nContact: <sip:phone@127.0.0.1:5061>\r\nMax-Forwards: 70\r\n{authorization}Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", body.len())
        };
        let challenged = engine.receive(invite("", 1).as_bytes(), peer, 0)?;
        let challenge = response(
            &challenged
                .datagrams
                .first()
                .ok_or(SipError::InvalidPacket)?
                .bytes,
        )?;
        if challenge.status_code.code() != 401 {
            return Err(SipError::InvalidPacket);
        }
        let challenge = WwwAuthenticate::parse(
            challenge
                .headers
                .iter()
                .find(|header| header.name().eq_ignore_ascii_case("WWW-Authenticate"))
                .ok_or(SipError::InvalidPacket)?
                .value(),
        )?;
        let uri = Uri::try_from("sip:100@localhost")?;
        let qop = AuthQop::Auth {
            cnonce: "refer-client".into(),
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
        let authorization = format!("Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"sip:100@localhost\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"refer-client\", nc=00000001\r\n", challenge.realm, challenge.nonce);
        let incoming = engine.receive(invite(&authorization, 2).as_bytes(), peer, 1)?;
        let id = incoming
            .events
            .into_iter()
            .find_map(|event| {
                if let SipEvent::Incoming { id, .. } = event {
                    Some(id)
                } else {
                    None
                }
            })
            .ok_or(SipError::InvalidPacket)?;
        let answered = engine.answer(&id, 7, "127.0.0.1:10000".parse()?, 2)?;
        let answer = response(
            &answered
                .datagrams
                .first()
                .ok_or(SipError::InvalidPacket)?
                .bytes,
        )?;
        let mut fixture = Self {
            engine,
            peer,
            id,
            answer,
        };
        fixture.receive("ACK", 2, "", 3)?;
        Ok(fixture)
    }

    pub fn packet(&self, method: &str, sequence: u32, extra: &str) -> Result<Vec<u8>, SipError> {
        Ok(format!("{method} sip:100@localhost SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-{method}-{sequence};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} {method}\r\nContact: <sip:phone@127.0.0.1:5061>\r\nMax-Forwards: 70\r\n{extra}Content-Length: 0\r\n\r\n", self.answer.from_header()?.value(), self.answer.to_header()?.value(), self.answer.call_id_header()?.value()).into_bytes())
    }

    pub fn receive(
        &mut self,
        method: &str,
        sequence: u32,
        extra: &str,
        now: u64,
    ) -> Result<Output, SipError> {
        self.engine
            .receive(&self.packet(method, sequence, extra)?, self.peer, now)
    }
}

pub(super) fn refer(output: Output) -> Result<DialogRefer, SipError> {
    output
        .events
        .into_iter()
        .find_map(|event| {
            if let SipEvent::BlindTransferRequested {
                id: _,
                generation: 7,
                refer,
            } = event
            {
                Some(refer)
            } else {
                None
            }
        })
        .ok_or(SipError::InvalidPacket)
}

pub(super) fn status(output: &Output, code: u16) -> bool {
    output.datagrams.iter().any(|packet| {
        packet
            .bytes
            .starts_with(format!("SIP/2.0 {code} ").as_bytes())
    })
}

pub(super) fn request(packet: &[u8]) -> Result<Request, SipError> {
    match SipMessage::try_from(packet)? {
        SipMessage::Request(request) => Ok(request),
        SipMessage::Response(_) => Err(SipError::InvalidPacket),
    }
}

fn response(packet: &[u8]) -> Result<Response, SipError> {
    match SipMessage::try_from(packet)? {
        SipMessage::Response(response) => Ok(response),
        SipMessage::Request(_) => Err(SipError::InvalidPacket),
    }
}

pub(super) fn notifies(output: &Output) -> Result<Vec<Request>, SipError> {
    let mut requests = Vec::new();
    for packet in &output.datagrams {
        if let SipMessage::Request(request) = SipMessage::try_from(packet.bytes.as_slice())? {
            if request.method != Method::Notify {
                return Err(SipError::InvalidPacket);
            }
            requests.push(request);
        }
    }
    Ok(requests)
}

pub(super) fn receipt(request: &Request, code: u16) -> Result<Vec<u8>, SipError> {
    Ok(format!("SIP/2.0 {code} Result\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n", request.top_via_header()?.value(), request.from_header()?.value(), request.to_header()?.value(), request.call_id_header()?.value(), request.cseq_header()?.value()).into_bytes())
}
