use crate::{Datagram, Engine, Output, Session, SipError, SipEvent};
use rsipstack::sip::{HeadersExt, Method, Request, Response, SipMessage, Uri};
use rusterisk_config::Codec;
use std::net::SocketAddr;

pub struct Outbound {
    pub id: String,
    pub generation: u64,
    pub endpoint: String,
    pub caller: String,
    pub local_rtp: SocketAddr,
    pub timeout_ms: u64,
}

enum State {
    Calling { retry: u64, interval: u64 },
    Proceeding,
    Established(Session),
    Cancelling { expires: u64 },
    Terminated { expires: u64 },
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
    deadline: u64,
    state: State,
    cseq: u32,
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
        format!("{}-{}-{cseq}", via.value(), method)
    } else {
        via.value().into()
    };
    let uri = if matches!(method, Method::Cancel) {
        &dialog.invite.uri
    } else {
        &dialog.remote_contact
    };
    let bytes = format!("{method} {uri} SIP/2.0\r\nVia: {via}\r\nFrom: {}\r\nTo: {to}\r\nCall-ID: {}\r\nCSeq: {cseq} {method}\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", dialog.invite.from_header()?.value(), dialog.invite.call_id_header()?.value());
    let parsed = request(&bytes)?;
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
    let mut body = format!("v=0\r\no=rusterisk {serial} {serial} IN {family} {}\r\ns=Rusterisk\r\nc=IN {family} {}\r\nt=0 0\r\nm=audio {} RTP/AVP {payloads} 101\r\n", local.ip(), local.ip(), local.port());
    for codec in codecs {
        body.push_str(match codec {
            Codec::Pcmu => "a=rtpmap:0 PCMU/8000\r\n",
            Codec::Pcma => "a=rtpmap:8 PCMA/8000\r\n",
            Codec::Opus => "a=rtpmap:111 opus/48000/2\r\na=fmtp:111 minptime=10;useinbandfec=1\r\n",
        });
    }
    body.push_str("a=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-16\r\na=sendrecv\r\n");
    body
}

impl Engine {
    pub fn originate(&mut self, outbound: Outbound, now: u64) -> Result<Output, SipError> {
        if self.outgoing.len() >= 64
            || outbound.id.is_empty()
            || outbound.id.len() > 128
            || outbound.caller.len() > 128
            || !outbound
                .caller
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+*-#._".contains(&byte))
            || outbound.timeout_ms == 0
            || outbound.timeout_ms > 3_600_000
        {
            return Err(SipError::Capacity);
        }
        if self
            .outgoing
            .values()
            .any(|dialog| dialog.id == outbound.id)
        {
            return Err(SipError::InvalidPacket);
        }
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
            deadline: now.saturating_add(outbound.timeout_ms),
            state: State::Calling {
                retry: now.saturating_add(500),
                interval: 500,
            },
            cseq: 1,
        };
        self.outgoing.insert(call_id, dialog);
        Ok(Output::response(peer, bytes.into_bytes()))
    }

    pub fn cancel(&mut self, id: &str, generation: u64, now: u64) -> Result<Output, SipError> {
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
            State::Calling { .. } | State::Proceeding => {
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
            State::Cancelling { .. } | State::Terminated { .. } => {
                return Ok(Output {
                    datagrams: Vec::new(),
                    events: Vec::new(),
                })
            }
        };
        Ok(Output::response(dialog.peer, packet))
    }

    pub fn tick(&mut self, now: u64) -> Result<Output, SipError> {
        let mut output = Output {
            datagrams: Vec::new(),
            events: Vec::new(),
        };
        self.outgoing.retain(|_, dialog| !matches!(dialog.state, State::Cancelling { expires } | State::Terminated { expires } if expires <= now));
        for dialog in self.outgoing.values_mut() {
            match &mut dialog.state {
                State::Calling { retry, interval } if dialog.deadline > now => {
                    if *retry <= now {
                        output.datagrams.push(Datagram {
                            peer: dialog.peer,
                            bytes: dialog.bytes.clone(),
                        });
                        *interval = (*interval * 2).min(4000);
                        *retry = now.saturating_add(*interval);
                    }
                }
                State::Calling { .. } | State::Proceeding if dialog.deadline <= now => {
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
        if dialog.peer != peer
            || response.cseq_header()?.value() != dialog.invite.cseq_header()?.value()
            || response.transaction_id()? != dialog.invite.transaction_id()?
            || response.from_header()?.value() != dialog.invite.from_header()?.value()
        {
            return Ok(output);
        }
        let code = response.status_code.code();
        if code < 200 {
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
                dialog.cseq,
                code < 300,
            )?,
        });
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
                State::Cancelling { .. } => {
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
        self.outgoing.values().find_map(|dialog| {
            if dialog.id == id && dialog.generation == generation {
                if let State::Established(session) = &dialog.state {
                    return Some(session);
                }
            }
            None
        })
    }
}
