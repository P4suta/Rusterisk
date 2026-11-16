use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
        untyped::ToTypedHeader,
    },
    services::DigestGenerator,
    HeadersExt, Method, Response, SipMessage, Uri,
};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_server::{
    CallKey, Control, ControlError, Effect, IncomingCall, IncomingTarget, Milestone, Signal,
};
use rusterisk_sip::{AttendedDialogRefer, Engine, Output, ReplacementDialog, Replaces, SipEvent};
use std::net::SocketAddr;

pub(super) fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

pub(super) fn configuration() -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test\n",
        "[source-udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[consultation-udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5070\n[shared]\ntype=auth\nusername=phone\npassword=password\nrealm=house\n[other]\ntype=auth\nusername=phone\npassword=password\nrealm=house\n[source-aor]\ntype=aor\n[consultation-aor]\ntype=aor\n[bob-aor]\ntype=aor\ncontact=sip:bob@127.0.0.1:5062\n[target-aor]\ntype=aor\ncontact=sip:target@127.0.0.1:5072\n[source]\ntype=endpoint\ntransport=source-udp\nauth=shared\naors=source-aor\ncontext=internal\nallow=ulaw\n[consultation]\ntype=endpoint\ntransport=consultation-udp\nauth=shared\naors=consultation-aor\ncontext=internal\nallow=ulaw\n[bob]\ntype=endpoint\ntransport=source-udp\nauth=shared\naors=bob-aor\ncontext=internal\nallow=ulaw\n[target]\ntype=endpoint\ntransport=consultation-udp\nauth=shared\naors=target-aor\ncontext=internal\nallow=ulaw\n",
        "", "[internal]\nexten => 100,1,Dial(PJSIP/bob,5)\nexten => 200,1,Dial(PJSIP/target,5)\n[outside]\nexten => 200,1,Dial(PJSIP/target,5)\n",
    ).map_err(failure)
}

pub(super) struct Dialog {
    pub source: CallKey,
    pub peer: CallKey,
    pub bridge: u64,
    answer: Response,
    address: SocketAddr,
}

pub(super) struct Fixture {
    pub control: Control,
    pub source: Dialog,
    pub consultation: Dialog,
    engines: [Engine; 2],
}

enum Selection {
    Source,
    Consultation,
}

impl Fixture {
    pub fn new() -> Result<Self, ControlError> {
        Self::with_consultation(configuration()?)
    }

    pub fn with_consultation(consultation: Config) -> Result<Self, ControlError> {
        let config = configuration()?;
        let mut source_config = config.clone();
        source_config
            .endpoints
            .retain(|endpoint| endpoint.name == "source");
        let mut consultation_config = consultation.clone();
        consultation_config
            .endpoints
            .retain(|endpoint| endpoint.name == "consultation");
        let mut engines = [
            Engine::new(source_config, [7; 32]),
            Engine::new(consultation_config, [11; 32]),
        ];
        let mut control = Control::with_configuration(16, &config);
        control.subscribe("calls")?;
        let source = establish(
            &mut control,
            &mut engines[0],
            &config,
            Invitation {
                endpoint: "source",
                extension: "100",
                call_id: "source-dialog",
                tag: "Source",
                address: "127.0.0.1:5061".parse().map_err(failure)?,
            },
            0,
        )?;
        let consultation = establish(
            &mut control,
            &mut engines[1],
            &consultation,
            Invitation {
                endpoint: "consultation",
                extension: "200",
                call_id: "consultation-dialog",
                tag: "Consultation",
                address: "127.0.0.1:5071".parse().map_err(failure)?,
            },
            10,
        )?;
        Ok(Self {
            control,
            source,
            consultation,
            engines,
        })
    }

    pub fn request(&mut self) -> Result<(AttendedDialogRefer, ReplacementDialog), ControlError> {
        self.request_for(Selection::Consultation)
    }

    pub fn request_same_pair(
        &mut self,
    ) -> Result<(AttendedDialogRefer, ReplacementDialog), ControlError> {
        self.request_for(Selection::Source)
    }

    fn request_for(
        &mut self,
        selection: Selection,
    ) -> Result<(AttendedDialogRefer, ReplacementDialog), ControlError> {
        let (answer, engine) = match selection {
            Selection::Source => (&self.source.answer, 0),
            Selection::Consultation => (&self.consultation.answer, 1),
        };
        let local = answer
            .to_header()
            .map_err(failure)?
            .typed()
            .map_err(failure)?;
        let remote = answer
            .from_header()
            .map_err(failure)?
            .typed()
            .map_err(failure)?;
        let selector = format!(
            "{};to-tag={};from-tag={}",
            answer.call_id_header().map_err(failure)?.value(),
            local
                .tag()
                .ok_or(ControlError::Invalid("Missing consultation local tag"))?
                .value(),
            remote
                .tag()
                .ok_or(ControlError::Invalid("Missing consultation remote tag"))?
                .value()
        );
        let replacement = self.engines[engine]
            .replacement_dialog(&Replaces::parse(&selector).map_err(failure)?)
            .map_err(|error| ControlError::Adapter(format!("{error:?}")))?
            .ok_or(ControlError::Invalid("Missing live consultation"))?;
        let encoded = selector.replace(';', "%3B").replace('=', "%3D");
        let output = self.engines[0]
            .receive(
                &related(
                    &self.source.answer,
                    self.source.address,
                    "REFER",
                    3,
                    &format!("Refer-To: <sip:target@localhost?Replaces={encoded}>\r\n"),
                )?,
                self.source.address,
                20,
            )
            .map_err(failure)?;
        assert!(output.datagrams.is_empty());
        let mut events = output.events.into_iter();
        let Some(SipEvent::AttendedTransferRequested {
            id,
            generation,
            refer,
        }) = events.next()
        else {
            return Err(ControlError::Invalid("Missing attended request"));
        };
        assert_eq!(id, self.source.source.id.as_str());
        assert_eq!(generation, self.source.source.generation);
        assert!(events.next().is_none());
        Ok((refer, replacement))
    }
}

struct Invitation {
    endpoint: &'static str,
    extension: &'static str,
    call_id: &'static str,
    tag: &'static str,
    address: SocketAddr,
}

impl Invitation {
    fn packet(&self, authorization: &str, sequence: u32) -> String {
        let body = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=call\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 10002 RTP/AVP 0\r\na=sendrecv\r\n";
        format!("INVITE sip:{}@localhost SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-{}-{sequence};rport\r\nFrom: Admin <sip:display-only@localhost>;tag={}\r\nTo: <sip:{}@localhost>\r\nCall-ID: {}\r\nCSeq: {sequence} INVITE\r\nContact: <sip:phone@{}>\r\nMax-Forwards: 70\r\n{authorization}Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", self.extension, self.address, self.tag, self.tag, self.extension, self.call_id, self.address, body.len())
    }
}

fn response(output: Output) -> Result<Response, ControlError> {
    let packet = output
        .datagrams
        .first()
        .ok_or(ControlError::Invalid("Missing SIP response"))?;
    match SipMessage::try_from(packet.bytes.as_slice()).map_err(failure)? {
        SipMessage::Response(response) => Ok(response),
        SipMessage::Request(_) => Err(ControlError::Invalid("Expected SIP response")),
    }
}

fn authenticated(
    engine: &mut Engine,
    config: &Config,
    invite: &Invitation,
    now: u64,
) -> Result<Output, ControlError> {
    let challenge = response(
        engine
            .receive(invite.packet("", 1).as_bytes(), invite.address, now)
            .map_err(failure)?,
    )?;
    assert_eq!(challenge.status_code.code(), 401);
    let challenge = WwwAuthenticate::parse(
        challenge
            .headers
            .iter()
            .find(|header| header.name().eq_ignore_ascii_case("WWW-Authenticate"))
            .ok_or(ControlError::Invalid("Missing challenge"))?
            .value(),
    )
    .map_err(failure)?;
    let endpoint = config
        .endpoints
        .iter()
        .find(|endpoint| endpoint.name == invite.endpoint)
        .ok_or(ControlError::Invalid("Missing endpoint"))?;
    let auth = config
        .auth
        .iter()
        .find(|auth| auth.name == endpoint.auth)
        .ok_or(ControlError::Invalid("Missing authentication profile"))?;
    let uri_text = format!("sip:{}@localhost", invite.extension);
    let uri = Uri::try_from(uri_text.as_str()).map_err(failure)?;
    let qop = AuthQop::Auth {
        cnonce: "attended-control".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username: &auth.username,
        password: auth.password.expose(),
        nonce: &challenge.nonce,
        uri: &uri,
        realm: &challenge.realm,
        method: &Method::Invite,
        qop: Some(&qop),
        algorithm: Algorithm::Md5,
    }
    .compute();
    let authorization = format!("Authorization: Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{uri_text}\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"attended-control\", nc=00000001\r\n", auth.username, challenge.realm, challenge.nonce);
    engine
        .receive(
            invite.packet(&authorization, 2).as_bytes(),
            invite.address,
            now + 1,
        )
        .map_err(failure)
}

fn establish(
    control: &mut Control,
    engine: &mut Engine,
    config: &Config,
    invite: Invitation,
    now: u64,
) -> Result<Dialog, ControlError> {
    let output = authenticated(engine, config, &invite, now)?;
    let mut events = output.events.into_iter();
    let Some(SipEvent::Incoming {
        id,
        endpoint,
        caller,
        context,
        extension,
        ..
    }) = events.next()
    else {
        return Err(ControlError::Invalid("Missing authenticated invitation"));
    };
    assert!(events.next().is_none());
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
    let started = control.start_dialplan(&source, config.dialplan.clone(), now + 1, "dial")?;
    let [Effect::Start { key: peer, .. }] = started.effects.as_slice() else {
        return Err(ControlError::Invalid("Expected one Dial branch"));
    };
    let peer = peer.clone();
    let answered = control.signal_at(
        Signal::Milestone {
            key: peer.clone(),
            milestone: Milestone::Answered,
        },
        "peer answer",
        now + 2,
    )?;
    assert!(answered
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::Answer { key } if key == &source)));
    let answer = response(
        engine
            .answer(
                source.id.as_str(),
                source.generation,
                "127.0.0.1:10000".parse().map_err(failure)?,
                now + 3,
            )
            .map_err(failure)?,
    )?;
    assert_eq!(answer.status_code.code(), 200);
    engine
        .receive(
            &related(&answer, invite.address, "ACK", 2, "")?,
            invite.address,
            now + 4,
        )
        .map_err(failure)?;
    let joined = control.signal_at(
        Signal::Milestone {
            key: source.clone(),
            milestone: Milestone::Answered,
        },
        "caller answer",
        now + 4,
    )?;
    let bridge = joined
        .effects
        .iter()
        .find_map(|effect| {
            if let Effect::DialBridge {
                parent,
                generation,
                peer: Some(selected),
            } = effect
            {
                assert_eq!(parent, &source);
                assert_eq!(selected, &peer);
                Some(*generation)
            } else {
                None
            }
        })
        .ok_or(ControlError::Invalid("Missing Dial bridge"))?;
    Ok(Dialog {
        source,
        peer,
        bridge,
        answer,
        address: invite.address,
    })
}

fn related(
    answer: &Response,
    address: SocketAddr,
    method: &str,
    sequence: u32,
    extra: &str,
) -> Result<Vec<u8>, ControlError> {
    let target = answer
        .contact_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?
        .uri;
    let text = format!("{method} {target} SIP/2.0\r\nVia: SIP/2.0/UDP {address};branch=z9hG4bK-{method}-{sequence};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} {method}\r\nContact: <sip:phone@{address}>\r\nMax-Forwards: 70\r\n{extra}Content-Length: 0\r\n\r\n", answer.from_header().map_err(failure)?.value(), answer.to_header().map_err(failure)?.value(), answer.call_id_header().map_err(failure)?.value());
    Ok(text.into_bytes())
}
