use crate::{Datagram, DialogReply, Engine, Output, Session, SipError, SipEvent};
use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop, Qop, Scheme},
        typed::{Authorization, WwwAuthenticate},
        untyped::ToTypedHeader,
    },
    services::DigestGenerator,
    Header, HeadersExt, Method, Request, Response, SipMessage, Uri,
};
use rusterisk_config::{Auth, Codec};
use std::net::SocketAddr;

pub struct Outbound {
    pub id: String,
    pub generation: u64,
    pub endpoint: String,
    pub caller: String,
    pub local_rtp: SocketAddr,
    pub timeout_ms: Option<u64>,
}

enum State {
    Calling {
        retry: u64,
        interval: u64,
    },
    CancelPending {
        retry: u64,
        interval: u64,
        expires: u64,
    },
    Proceeding,
    Established(Session),
    Cancelling {
        expires: u64,
    },
    Terminated {
        expires: u64,
    },
}

pub(crate) struct Outgoing {
    id: String,
    generation: u64,
    peer: SocketAddr,
    invite: Request,
    bytes: Vec<u8>,
    remote_to: String,
    remote_contact: Uri,
    codecs: Vec<Codec>,
    deadline: Option<u64>,
    transaction_deadline: u64,
    state: State,
    cseq: u32,
    remote_cseq: Option<u32>,
    credentials: Vec<Auth>,
    challenges: Vec<Challenge>,
}

struct Challenge {
    response: Response,
    ack: Vec<u8>,
    expires: u64,
}

fn authenticate(
    dialog: &mut Outgoing,
    response: &Response,
    now: u64,
) -> Result<Option<Vec<u8>>, SipError> {
    if dialog.challenges.len() >= 4
        || dialog.deadline.is_some_and(|deadline| deadline <= now)
        || !matches!(dialog.state, State::Calling { .. } | State::Proceeding)
    {
        return Ok(None);
    }
    let proxy = match response.status_code.code() {
        401 => false,
        407 => true,
        _ => return Ok(None),
    };
    let name = if proxy {
        "Proxy-Authenticate"
    } else {
        "WWW-Authenticate"
    };
    let Some(header) = response
        .headers
        .iter()
        .find(|header| header.name().eq_ignore_ascii_case(name))
    else {
        return Ok(None);
    };
    let challenge = match WwwAuthenticate::parse(header.value()) {
        Ok(challenge) => challenge,
        Err(_) => return Ok(None),
    };
    if challenge.scheme != Scheme::Digest
        || challenge.nonce.len() > 256
        || challenge.realm.len() > 256
        || challenge
            .opaque
            .as_ref()
            .is_some_and(|opaque| opaque.len() > 256)
    {
        return Ok(None);
    }
    let algorithm = match challenge.algorithm.unwrap_or(Algorithm::Md5) {
        Algorithm::Md5 => Algorithm::Md5,
        Algorithm::Md5Sess => Algorithm::Md5Sess,
        Algorithm::Sha256 | Algorithm::Sha256Sess | Algorithm::Sha512 | Algorithm::Sha512Sess => {
            return Ok(None)
        }
    };
    let authorization_name = if proxy {
        "Proxy-Authorization"
    } else {
        "Authorization"
    };
    if let Some(previous) = dialog
        .invite
        .headers
        .iter()
        .find(|header| header.name().eq_ignore_ascii_case(authorization_name))
    {
        let previous = Authorization::parse(previous.value())?;
        if challenge.stale.as_deref() != Some("true") || previous.nonce == challenge.nonce {
            return Ok(None);
        }
    }
    let Some(credentials) = dialog
        .credentials
        .iter()
        .find(|auth| auth.realm == challenge.realm)
        .or_else(|| {
            dialog
                .credentials
                .iter()
                .find(|auth| auth.realm.is_empty() || auth.realm == "*")
        })
    else {
        return Ok(None);
    };
    let qop = match challenge.qop {
        Some(Qop::Auth) => {
            let mut bytes = [0; 16];
            getrandom::fill(&mut bytes).map_err(|_| SipError::Entropy)?;
            Some(AuthQop::Auth {
                cnonce: format!("{:032x}", u128::from_be_bytes(bytes)),
                nc: 1,
            })
        }
        Some(Qop::AuthInt) => return Ok(None),
        None if algorithm == Algorithm::Md5Sess => return Ok(None),
        None => None,
    };
    let digest = DigestGenerator {
        username: &credentials.username,
        password: credentials.password.expose(),
        nonce: &challenge.nonce,
        uri: &dialog.invite.uri,
        realm: &challenge.realm,
        method: &Method::Invite,
        qop: qop.as_ref(),
        algorithm,
    }
    .compute();
    let authorization = Authorization {
        scheme: Scheme::Digest,
        username: credentials.username.clone(),
        realm: challenge.realm,
        nonce: challenge.nonce,
        uri: dialog.invite.uri.clone(),
        response: digest,
        algorithm: Some(algorithm),
        opaque: challenge.opaque,
        qop,
    };
    let mut invite = dialog.invite.clone();
    let sequence = dialog
        .cseq
        .checked_add(1)
        .filter(|sequence| *sequence < 1 << 31)
        .ok_or(SipError::Capacity)?;
    let via = invite.top_via_header()?.typed()?;
    let branch = format!(
        "{}-auth-{sequence}",
        via.branch().ok_or(SipError::InvalidPacket)?
    );
    let header = invite
        .headers
        .iter_mut()
        .find(|header| header.name().eq_ignore_ascii_case("Via"))
        .ok_or(SipError::InvalidPacket)?;
    *header = Header::Via(rsipstack::sip::headers::untyped::Via::new(
        via.with_branch(branch.into()).to_string(),
    ));
    invite
        .cseq_header_mut()?
        .replace(format!("{sequence} INVITE"));
    let header = if proxy {
        Header::ProxyAuthorization(rsipstack::sip::headers::untyped::ProxyAuthorization::new(
            authorization.to_string(),
        ))
    } else {
        authorization.into()
    };
    invite.headers.unique_push(header);
    let bytes = invite.to_string().into_bytes();
    if bytes.len() > crate::MAX_MESSAGE {
        return Err(SipError::Capacity);
    }
    dialog.cseq = sequence;
    dialog.invite = invite;
    dialog.bytes = bytes.clone();
    dialog.state = State::Calling {
        retry: now.saturating_add(500),
        interval: 500,
    };
    dialog.transaction_deadline = now.saturating_add(32_000);
    Ok(Some(bytes))
}

fn request(bytes: &str) -> Result<Request, SipError> {
    match SipMessage::try_from(bytes)? {
        SipMessage::Request(request) => Ok(request),
        SipMessage::Response(_) => Err(SipError::InvalidPacket),
    }
}

fn related(
    dialog: &Outgoing,
    method: Method,
    to: &str,
    cseq: u32,
    fresh_branch: bool,
) -> Result<Vec<u8>, SipError> {
    let via = dialog.invite.top_via_header()?;
    let via = if fresh_branch {
        let via = via.typed()?;
        let branch = via.branch().ok_or(SipError::InvalidPacket)?;
        let branch = format!("{branch}-{method}-{cseq}");
        via.with_branch(branch.into()).to_string()
    } else {
        via.value().into()
    };
    let uri = if matches!(method, Method::Cancel) || (method == Method::Ack && !fresh_branch) {
        &dialog.invite.uri
    } else {
        &dialog.remote_contact
    };
    let bytes = format!("{method} {uri} SIP/2.0\r\nVia: {via}\r\nFrom: {}\r\nTo: {to}\r\nCall-ID: {}\r\nCSeq: {cseq} {method}\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", dialog.invite.from_header()?.value(), dialog.invite.call_id_header()?.value());
    let mut parsed = request(&bytes)?;
    if method == Method::Ack {
        for header in dialog.invite.headers.iter().filter(|header| {
            header.name().eq_ignore_ascii_case("Authorization")
                || header.name().eq_ignore_ascii_case("Proxy-Authorization")
        }) {
            parsed.headers.push(header.clone());
        }
    }
    Ok(parsed.to_string().into_bytes())
}

fn formats(codecs: &[Codec], local: SocketAddr, serial: u64) -> String {
    let family = if local.is_ipv4() { "IP4" } else { "IP6" };
    let payloads = codecs
        .iter()
        .map(|codec| match codec {
            Codec::Pcmu => "0",
            Codec::Pcma => "8",
            Codec::Opus => "111",
        })
        .collect::<Vec<_>>()
        .join(" ");
    let events = if codecs.contains(&Codec::Opus) {
        "101 102"
    } else {
        "101"
    };
    let mut body = format!("v=0\r\no=rusterisk {serial} {serial} IN {family} {}\r\ns=Rusterisk\r\nc=IN {family} {}\r\nt=0 0\r\nm=audio {} RTP/AVP {payloads} {events}\r\n", local.ip(), local.ip(), local.port());
    for codec in codecs {
        body.push_str(match codec {
            Codec::Pcmu => "a=rtpmap:0 PCMU/8000\r\n",
            Codec::Pcma => "a=rtpmap:8 PCMA/8000\r\n",
            Codec::Opus => "a=rtpmap:111 opus/48000/2\r\na=fmtp:111 minptime=10;useinbandfec=1\r\n",
        });
    }
    body.push_str("a=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\n");
    if codecs.contains(&Codec::Opus) {
        body.push_str("a=rtpmap:102 telephone-event/48000\r\na=fmtp:102 0-15\r\n");
    }
    body.push_str("a=sendrecv\r\n");
    body
}

impl Engine {
    pub(crate) fn outgoing_info(
        &mut self,
        id: &str,
        generation: u64,
        digit: rusterisk_core::DtmfDigit,
        duration_ms: u32,
    ) -> Result<Option<Datagram>, SipError> {
        let Some(dialog) = self
            .outgoing
            .values_mut()
            .find(|dialog| dialog.id == id && dialog.generation == generation)
        else {
            return Ok(None);
        };
        if !matches!(dialog.state, State::Established(_)) {
            return Err(SipError::InvalidPacket);
        }
        dialog.cseq = dialog
            .cseq
            .checked_add(1)
            .filter(|sequence| *sequence < 1 << 31)
            .ok_or(SipError::Capacity)?;
        let packet = related(dialog, Method::Info, &dialog.remote_to, dialog.cseq, true)?;
        let request = request(std::str::from_utf8(&packet)?)?;
        let bytes = crate::info::encode(request, digit, duration_ms)?;
        Ok(Some(Datagram {
            peer: dialog.peer,
            bytes,
        }))
    }

    pub fn originate(&mut self, outbound: Outbound, now: u64) -> Result<Output, SipError> {
        if self.outgoing.len() >= 64
            || outbound.id.is_empty()
            || outbound.id.len() > 128
            || outbound.caller.len() > 128
            || !outbound
                .caller
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+*-#._".contains(&byte))
            || outbound
                .timeout_ms
                .is_some_and(|timeout| timeout == 0 || timeout > 65_535_000)
        {
            return Err(SipError::Capacity);
        }
        if self
            .outgoing
            .values()
            .any(|dialog| dialog.id == outbound.id && dialog.generation == outbound.generation)
        {
            return Err(SipError::InvalidPacket);
        }
        let deadline = outbound
            .timeout_ms
            .map(|timeout| now.checked_add(timeout).ok_or(SipError::Capacity))
            .transpose()?;
        let transaction_deadline = now.checked_add(32_000).ok_or(SipError::Capacity)?;
        let endpoint = self
            .config
            .endpoints
            .iter()
            .find(|endpoint| endpoint.name == outbound.endpoint)
            .ok_or(SipError::InvalidPacket)?
            .clone();
        let transport = self
            .config
            .transports
            .iter()
            .find(|transport| transport.name == endpoint.transport)
            .ok_or(SipError::InvalidPacket)?;
        let registered = self
            .contacts
            .get(&endpoint.name)
            .filter(|(_, expires)| *expires > now)
            .map(|(peer, _)| *peer);
        let contact = self
            .config
            .aors
            .iter()
            .filter(|aor| endpoint.aors.contains(&aor.name))
            .find_map(|aor| aor.contact.as_deref());
        let uri = match (registered, contact) {
            (Some(peer), _) => Uri::try_from(format!("sip:{}@{peer}", endpoint.name))?,
            (None, Some(contact)) => Uri::try_from(contact)?,
            (None, None) => return Err(SipError::InvalidPacket),
        };
        let peer = match registered {
            Some(peer) => peer,
            None => uri.host_with_port.to_string().parse::<SocketAddr>()?,
        };
        let signaling = transport.bind;
        let token = self.token()?;
        let body = formats(&endpoint.codecs, outbound.local_rtp, self.serial);
        let caller = if outbound.caller.is_empty() {
            "rusterisk"
        } else {
            &outbound.caller
        };
        let bytes = format!("INVITE {uri} SIP/2.0\r\nVia: SIP/2.0/UDP {signaling};branch=z9hG4bK-{token};rport\r\nFrom: <sip:{caller}@{}>;tag={token}\r\nTo: <{uri}>\r\nCall-ID: {token}@rusterisk\r\nCSeq: 1 INVITE\r\nContact: <sip:rusterisk@{signaling}>\r\nMax-Forwards: 70\r\nAllow: INVITE, ACK, CANCEL, BYE, OPTIONS, INFO, REFER\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", signaling.ip(), body.len());
        let invite = request(&bytes)?;
        let call_id = invite.call_id_header()?.value().to_owned();
        let dialog = Outgoing {
            id: outbound.id,
            generation: outbound.generation,
            peer,
            remote_to: invite.to_header()?.value().into(),
            remote_contact: uri,
            invite,
            bytes: bytes.as_bytes().to_vec(),
            codecs: endpoint.codecs,
            deadline,
            transaction_deadline,
            state: State::Calling {
                retry: now.saturating_add(500),
                interval: 500,
            },
            cseq: 1,
            remote_cseq: None,
            credentials: endpoint
                .outbound_auth
                .iter()
                .filter_map(|name| self.config.auth.iter().find(|auth| &auth.name == name))
                .cloned()
                .collect(),
            challenges: Vec::new(),
        };
        self.outgoing.insert(call_id, dialog);
        Ok(Output::response(peer, bytes.into_bytes()))
    }

    pub fn cancel(&mut self, id: &str, generation: u64, now: u64) -> Result<Output, SipError> {
        let output = self.cancel_inner(id, generation, now)?;
        self.track_output(&output, now)?;
        Ok(output)
    }

    fn cancel_inner(&mut self, id: &str, generation: u64, now: u64) -> Result<Output, SipError> {
        if let Some(output) = self.hangup_incoming(id, generation, now)? {
            return Ok(output);
        }
        let Some(dialog) = self
            .outgoing
            .values_mut()
            .find(|dialog| dialog.id == id && dialog.generation == generation)
        else {
            return Ok(Output {
                datagrams: Vec::new(),
                events: Vec::new(),
            });
        };
        let packet = match dialog.state {
            State::Calling { retry, interval } => {
                dialog.state = State::CancelPending {
                    retry,
                    interval,
                    expires: dialog.transaction_deadline,
                };
                return Ok(Output {
                    datagrams: Vec::new(),
                    events: Vec::new(),
                });
            }
            State::Proceeding => {
                let packet = related(
                    dialog,
                    Method::Cancel,
                    dialog.invite.to_header()?.value(),
                    dialog.cseq,
                    false,
                )?;
                dialog.state = State::Cancelling {
                    expires: now.saturating_add(32_000),
                };
                packet
            }
            State::Established(_) => {
                dialog.cseq = dialog.cseq.checked_add(1).ok_or(SipError::Capacity)?;
                let packet = related(dialog, Method::Bye, &dialog.remote_to, dialog.cseq, true)?;
                dialog.state = State::Terminated {
                    expires: now.saturating_add(32_000),
                };
                packet
            }
            State::CancelPending { .. } | State::Cancelling { .. } | State::Terminated { .. } => {
                return Ok(Output {
                    datagrams: Vec::new(),
                    events: Vec::new(),
                })
            }
        };
        Ok(Output::response(dialog.peer, packet))
    }

    pub fn tick(&mut self, now: u64) -> Result<Output, SipError> {
        let mut output = self.tick_inner(now)?;
        self.track_output(&output, now)?;
        output.datagrams.extend(self.transactions.tick(now));
        Ok(output)
    }

    fn tick_inner(&mut self, now: u64) -> Result<Output, SipError> {
        let mut output = self.incoming_tick(now)?;
        self.outgoing.retain(|_, dialog| !matches!(dialog.state, State::CancelPending { expires, .. } | State::Cancelling { expires } | State::Terminated { expires } if expires <= now));
        for dialog in self.outgoing.values_mut() {
            dialog
                .challenges
                .retain(|challenge| challenge.expires > now);
            match &mut dialog.state {
                State::Calling { retry, interval }
                    if dialog
                        .deadline
                        .map(|deadline| deadline.min(dialog.transaction_deadline))
                        .unwrap_or(dialog.transaction_deadline)
                        > now =>
                {
                    if *retry <= now {
                        output.datagrams.push(Datagram {
                            peer: dialog.peer,
                            bytes: dialog.bytes.clone(),
                        });
                        *interval = (*interval * 2).min(32_000);
                        *retry = now.saturating_add(*interval);
                    }
                }
                State::CancelPending {
                    retry, interval, ..
                } => {
                    if *retry <= now {
                        output.datagrams.push(Datagram {
                            peer: dialog.peer,
                            bytes: dialog.bytes.clone(),
                        });
                        *interval = (*interval * 2).min(32_000);
                        *retry = now.saturating_add(*interval);
                    }
                }
                State::Calling { retry, interval }
                    if dialog
                        .deadline
                        .map(|deadline| deadline.min(dialog.transaction_deadline))
                        .unwrap_or(dialog.transaction_deadline)
                        <= now =>
                {
                    output.events.push(SipEvent::Ended {
                        id: dialog.id.clone(),
                        generation: dialog.generation,
                        cause: 19,
                    });
                    dialog.state = if dialog.transaction_deadline <= now {
                        State::Terminated {
                            expires: now.saturating_add(32_000),
                        }
                    } else {
                        State::CancelPending {
                            retry: *retry,
                            interval: *interval,
                            expires: dialog.transaction_deadline,
                        }
                    };
                }
                State::Proceeding if dialog.deadline.is_some_and(|deadline| deadline <= now) => {
                    output.datagrams.push(Datagram {
                        peer: dialog.peer,
                        bytes: related(
                            dialog,
                            Method::Cancel,
                            dialog.invite.to_header()?.value(),
                            dialog.cseq,
                            false,
                        )?,
                    });
                    output.events.push(SipEvent::Ended {
                        id: dialog.id.clone(),
                        generation: dialog.generation,
                        cause: 19,
                    });
                    dialog.state = State::Cancelling {
                        expires: now.saturating_add(32_000),
                    };
                }
                State::Calling { .. }
                | State::Proceeding
                | State::Established(_)
                | State::Cancelling { .. }
                | State::Terminated { .. } => {}
            }
        }
        Ok(output)
    }

    pub(crate) fn receive_response(
        &mut self,
        response: Response,
        peer: SocketAddr,
        now: u64,
    ) -> Result<Output, SipError> {
        let mut output = Output {
            datagrams: Vec::new(),
            events: Vec::new(),
        };
        let Some(dialog) = self.outgoing.get_mut(response.call_id_header()?.value()) else {
            return Ok(output);
        };
        if dialog.peer == peer {
            if let Some(challenge) = dialog
                .challenges
                .iter()
                .find(|challenge| challenge.expires > now && challenge.response == response)
            {
                return Ok(Output::response(peer, challenge.ack.clone()));
            }
        }
        if dialog.peer != peer
            || response.cseq_header()?.value() != dialog.invite.cseq_header()?.value()
            || response.transaction_id()? != dialog.invite.transaction_id()?
            || response.from_header()?.value() != dialog.invite.from_header()?.value()
        {
            return Ok(output);
        }
        let code = response.status_code.code();
        if code < 200 {
            if let State::CancelPending { expires, .. } = dialog.state {
                output.datagrams.push(Datagram {
                    peer,
                    bytes: related(
                        dialog,
                        Method::Cancel,
                        dialog.invite.to_header()?.value(),
                        dialog.cseq,
                        false,
                    )?,
                });
                dialog.state = State::Cancelling { expires };
                return Ok(output);
            }
            if matches!(dialog.state, State::Calling { .. }) {
                if code >= 180 {
                    output.events.push(SipEvent::Ringing {
                        id: dialog.id.clone(),
                        generation: dialog.generation,
                    });
                }
                dialog.state = State::Proceeding;
            }
            return Ok(output);
        }
        dialog.remote_to = response.to_header()?.value().into();
        if let Ok(contact) = response.contact_header() {
            let contact = contact
                .value()
                .trim()
                .trim_start_matches('<')
                .split('>')
                .next()
                .ok_or(SipError::InvalidPacket)?;
            dialog.remote_contact = Uri::try_from(contact)?;
        }
        output.datagrams.push(Datagram {
            peer,
            bytes: related(
                dialog,
                Method::Ack,
                &dialog.remote_to,
                dialog.invite.cseq_header()?.typed()?.seq,
                code < 300,
            )?,
        });
        if let Some(bytes) = authenticate(dialog, &response, now)? {
            dialog.challenges.push(Challenge {
                response,
                ack: output.datagrams[0].bytes.clone(),
                expires: now.saturating_add(32_000),
            });
            output.datagrams.push(Datagram { peer, bytes });
            return Ok(output);
        }
        if code < 300 {
            match dialog.state {
                State::Calling { .. } | State::Proceeding => {
                    let session =
                        Session::parse(std::str::from_utf8(&response.body)?, &dialog.codecs)?;
                    output.events.push(SipEvent::Answered {
                        id: dialog.id.clone(),
                        generation: dialog.generation,
                        session: session.clone(),
                    });
                    dialog.state = State::Established(session);
                }
                State::CancelPending { .. } | State::Cancelling { .. } => {
                    dialog.cseq = dialog.cseq.checked_add(1).ok_or(SipError::Capacity)?;
                    output.datagrams.push(Datagram {
                        peer,
                        bytes: related(dialog, Method::Bye, &dialog.remote_to, dialog.cseq, true)?,
                    });
                    dialog.state = State::Terminated {
                        expires: now.saturating_add(32_000),
                    };
                }
                State::Established(_) | State::Terminated { .. } => {}
            }
        } else {
            if matches!(dialog.state, State::Calling { .. } | State::Proceeding) {
                output.events.push(SipEvent::Ended {
                    id: dialog.id.clone(),
                    generation: dialog.generation,
                    cause: if code == 486 { 17 } else { 21 },
                });
            }
            dialog.state = State::Terminated {
                expires: now.saturating_add(32_000),
            };
        }
        Ok(output)
    }

    pub fn active_session(&self, id: &str, generation: u64) -> Option<&Session> {
        if let Some(session) = self.incoming_session(id, generation) {
            return Some(session);
        }
        self.outgoing.values().find_map(|dialog| {
            if dialog.id == id && dialog.generation == generation {
                if let State::Established(session) = &dialog.state {
                    return Some(session);
                }
            }
            None
        })
    }

    pub(crate) fn outgoing_request(
        &mut self,
        request: &Request,
        peer: SocketAddr,
        now: u64,
    ) -> Result<Option<DialogReply>, SipError> {
        if !matches!(request.method, Method::Bye | Method::Info | Method::Invite) {
            return Ok(None);
        }
        let Some(dialog) = self.outgoing.get_mut(request.call_id_header()?.value()) else {
            return Ok(None);
        };
        let remote = rsipstack::sip::headers::typed::To::parse(&dialog.remote_to)?;
        if dialog.peer != peer
            || remote.tag().is_none()
            || request.from_header()?.typed()?.tag() != remote.tag()
            || request.to_header()?.typed()?.tag() != dialog.invite.from_header()?.typed()?.tag()
        {
            return Ok(None);
        }
        let sequence = request.cseq_header()?.typed()?.seq;
        if dialog
            .remote_cseq
            .is_some_and(|previous| sequence <= previous)
        {
            return Ok(Some(DialogReply::Rejected(crate::response(
                request,
                500,
                "Server Internal Error",
                "",
                "",
            )?)));
        }
        if request.method == Method::Invite {
            let (code, phrase) = if matches!(dialog.state, State::Established(_)) {
                (501, "Not Implemented")
            } else {
                (481, "Call/Transaction Does Not Exist")
            };
            return Ok(Some(DialogReply::Rejected(crate::response(
                request, code, phrase, "", "",
            )?)));
        }
        dialog.remote_cseq = Some(sequence);
        if request.method == Method::Info {
            if !matches!(dialog.state, State::Established(_)) {
                return Ok(Some(DialogReply::Rejected(crate::response(
                    request,
                    481,
                    "Call/Transaction Does Not Exist",
                    "",
                    "",
                )?)));
            }
            return crate::info::receive(request, &dialog.id, dialog.generation).map(Some);
        }
        let mut events = Vec::new();
        match dialog.state {
            State::Established(_) => events.push(SipEvent::Ended {
                id: dialog.id.clone(),
                generation: dialog.generation,
                cause: 16,
            }),
            State::Calling { .. }
            | State::CancelPending { .. }
            | State::Proceeding
            | State::Cancelling { .. }
            | State::Terminated { .. } => {}
        }
        dialog.state = State::Terminated {
            expires: now.saturating_add(32_000),
        };
        Ok(Some(DialogReply::Accepted {
            bytes: crate::response(request, 200, "OK", "", "")?,
            events,
        }))
    }
}
