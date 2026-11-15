use crate::{
    response, Datagram, DialogReply, Engine, Output, PendingInvite, Session, SipError, SipEvent,
};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method, Request, SipMessage};
use std::net::SocketAddr;

enum State {
    AwaitingAck {
        sequence: u32,
        retry: u64,
        interval: u64,
        deadline: u64,
    },
    Established,
    Terminated {
        expires: u64,
    },
}

pub(crate) struct Incoming {
    invite: PendingInvite,
    generation: u64,
    bytes: Vec<u8>,
    state: State,
    local_cseq: u32,
    remote_cseq: u32,
    local_media: SocketAddr,
    answer_version: u64,
    offer: Vec<u8>,
    remote_target: rsipstack::sip::Uri,
}

impl Incoming {
    fn matches(&self, request: &Request, peer: SocketAddr) -> Result<bool, SipError> {
        Ok(self.invite.peer == peer
            && request.call_id_header()?.value() == self.invite.request.call_id_header()?.value()
            && request.from_header()?.typed()?.tag()
                == self.invite.request.from_header()?.typed()?.tag()
            && request.to_header()?.typed()?.tag().map(|tag| tag.value())
                == Some(self.invite.local_tag.as_str()))
    }

    fn request(&mut self, method: Method) -> Result<Request, SipError> {
        self.local_cseq = self
            .local_cseq
            .checked_add(1)
            .filter(|sequence| *sequence < 1 << 31)
            .ok_or(SipError::Capacity)?;
        let invite = tagged(&self.invite.request, &self.invite.local_tag)?;
        let target = &self.remote_target;
        let packet = format!("{method} {target} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-{}-{method}-{};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {} {method}\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", self.invite.local, self.invite.local_tag, self.local_cseq, invite.to_header()?.value(), invite.from_header()?.value(), invite.call_id_header()?.value(), self.local_cseq);
        match SipMessage::try_from(packet.as_str())? {
            SipMessage::Request(request) => Ok(request),
            SipMessage::Response(_) => Err(SipError::InvalidPacket),
        }
    }

    fn bye(&mut self, now: u64) -> Result<Vec<u8>, SipError> {
        let packet = self.request(Method::Bye)?.to_string().into_bytes();
        self.state = State::Terminated {
            expires: now.saturating_add(32_000),
        };
        Ok(packet)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    NoRoute,
    Busy,
    Rejected,
    Unsupported,
}

impl RejectReason {
    const fn response(self) -> (u16, &'static str) {
        match self {
            Self::NoRoute => (404, "Not Found"),
            Self::Busy => (486, "Busy Here"),
            Self::Rejected => (603, "Decline"),
            Self::Unsupported => (501, "Not Implemented"),
        }
    }
}

pub(super) fn tagged(request: &Request, tag: &str) -> Result<Request, SipError> {
    let mut request = request.clone();
    let to = request.to_header()?.typed()?.with_tag(tag.into());
    request.to_header_mut()?.replace(to.to_string());
    Ok(request)
}

fn same_invite(
    invite: &PendingInvite,
    request: &Request,
    peer: SocketAddr,
) -> Result<bool, SipError> {
    Ok(invite.peer == peer
        && invite.request.call_id_header()?.value() == request.call_id_header()?.value()
        && invite.request.uri == request.uri
        && invite.request.cseq_header()?.typed()?.seq == request.cseq_header()?.typed()?.seq
        && invite.request.top_via_header()?.typed()? == request.top_via_header()?.typed()?
        && invite.request.from_header()?.typed()? == request.from_header()?.typed()?
        && invite.request.to_header()?.typed()? == request.to_header()?.typed()?)
}

fn origin(source: &str) -> Result<([&str; 5], u64), SipError> {
    let mut origins = source.lines().filter_map(|line| line.strip_prefix("o="));
    let line = origins.next().ok_or(SipError::InvalidPacket)?;
    if origins.next().is_some() {
        return Err(SipError::InvalidPacket);
    }
    let fields: Vec<&str> = line.split_whitespace().take(7).collect();
    let [user, id, version, network, family, address] = fields.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    id.parse::<i64>()
        .ok()
        .filter(|id| *id >= 0)
        .ok_or(SipError::InvalidPacket)?;
    let version = version
        .parse::<i64>()
        .ok()
        .filter(|version| *version >= 0)
        .ok_or(SipError::InvalidPacket)?;
    Ok(([*user, *id, *network, *family, *address], version as u64))
}

fn unchanged_field(line: &&str) -> bool {
    !line.starts_with("o=")
        && !matches!(
            *line,
            "a=sendrecv" | "a=sendonly" | "a=recvonly" | "a=inactive"
        )
}

fn direction_update(
    previous: &[u8],
    offered: &[u8],
    session: &Session,
) -> Result<Session, SipError> {
    let previous = std::str::from_utf8(previous)?;
    let offered = std::str::from_utf8(offered)?;
    let (old_identity, old_version) = origin(previous)?;
    let (identity, version) = origin(offered)?;
    if identity != old_identity
        || (version == old_version && previous != offered)
        || (version != old_version && old_version.checked_add(1) != Some(version))
        || !previous
            .lines()
            .filter(unchanged_field)
            .eq(offered.lines().filter(unchanged_field))
    {
        return Err(SipError::Unsupported);
    }
    let updated = Session::parse(offered, &[session.codec])?;
    if updated.remote != session.remote
        || updated.codec != session.codec
        || updated.payload_type != session.payload_type
        || updated.telephone != session.telephone
    {
        return Err(SipError::Unsupported);
    }
    Ok(updated)
}

impl Engine {
    pub(crate) fn incoming_info(
        &mut self,
        id: &str,
        generation: u64,
        digit: rusterisk_core::DtmfDigit,
        duration_ms: u32,
    ) -> Result<Option<Datagram>, SipError> {
        let Some(dialog) = self
            .incoming
            .values_mut()
            .find(|dialog| dialog.invite.id == id && dialog.generation == generation)
        else {
            return Ok(None);
        };
        if !matches!(dialog.state, State::AwaitingAck { .. } | State::Established) {
            return Err(SipError::InvalidPacket);
        }
        let request = dialog.request(Method::Info)?;
        let bytes = crate::info::encode(request, digit, duration_ms)?;
        Ok(Some(Datagram {
            peer: dialog.invite.peer,
            bytes,
        }))
    }

    pub(crate) fn incoming_cancel(
        &mut self,
        key: String,
        request: Request,
        peer: SocketAddr,
        now: u64,
    ) -> Result<Output, SipError> {
        let mut invite = None;
        for candidate in self
            .pending
            .values()
            .chain(self.incoming.values().map(|dialog| &dialog.invite))
        {
            if same_invite(candidate, &request, peer)? {
                invite = Some(candidate);
                break;
            }
        }
        let Some(invite) = invite else {
            return Ok(Output::response(
                peer,
                self.render_response(&request, 481, "Call/Transaction Does Not Exist", "", "")?,
            ));
        };
        let id = invite.id.clone();
        let bytes = response(&tagged(&request, &invite.local_tag)?, 200, "OK", "", "")?;
        let mut output = self.cache_reply(key, request, peer, now, bytes, Vec::new())?;
        if let Some(invite) = self.pending.remove(&id) {
            let bytes = response(
                &tagged(&invite.request, &invite.local_tag)?,
                487,
                "Request Terminated",
                "",
                "",
            )?;
            self.update_invite_reply(&invite, &bytes, now)?;
            output.datagrams.push(Datagram { peer, bytes });
            output.events.push(SipEvent::IncomingEnded {
                id: invite.id,
                cause: 16,
            });
        }
        Ok(output)
    }

    fn update_invite_reply(
        &mut self,
        invite: &PendingInvite,
        bytes: &[u8],
        now: u64,
    ) -> Result<(), SipError> {
        for cached in self
            .cache
            .values_mut()
            .filter(|cached| cached.peer == invite.peer && cached.request == invite.request)
        {
            cached.bytes = bytes.into();
            cached.expires = Some(now.saturating_add(32_000));
            cached.retry = crate::ServerRetry::for_reply(&cached.request, bytes, now)?;
        }
        Ok(())
    }

    pub fn ring(&mut self, id: &str) -> Result<Output, SipError> {
        let invite = self
            .pending
            .values()
            .find(|invite| invite.id == id)
            .ok_or(SipError::InvalidPacket)?;
        let bytes = response(
            &tagged(&invite.request, &invite.local_tag)?,
            180,
            "Ringing",
            "",
            "",
        )?;
        let peer = invite.peer;
        for cached in self
            .cache
            .values_mut()
            .filter(|cached| cached.peer == peer && cached.request == invite.request)
        {
            cached.bytes = bytes.clone();
            cached.expires = None;
        }
        Ok(Output::response(peer, bytes))
    }

    pub fn answer(
        &mut self,
        id: &str,
        generation: u64,
        local: SocketAddr,
        now: u64,
    ) -> Result<Output, SipError> {
        if generation == 0 || local.port() == 0 || self.incoming.len() >= 64 {
            return Err(SipError::Capacity);
        }
        let (key, invite) = self
            .pending
            .iter()
            .find(|(_, invite)| invite.id == id)
            .ok_or(SipError::InvalidPacket)?;
        let key = key.clone();
        let extra = format!("Contact: <sip:rusterisk@{}>\r\nAllow: INVITE, ACK, CANCEL, BYE, OPTIONS\r\nSupported: \r\n", invite.local);
        let bytes = response(
            &tagged(&invite.request, &invite.local_tag)?,
            200,
            "OK",
            &extra,
            &invite.session.answer(local, generation),
        )?;
        let sequence = invite.request.cseq_header()?.typed()?.seq;
        let remote_target = invite.request.contact_header()?.typed()?.uri;
        let offer = invite.request.body.clone();
        let invite = self.pending.remove(&key).ok_or(SipError::InvalidPacket)?;
        self.update_invite_reply(&invite, &bytes, now)?;
        let mut output = Output::response(invite.peer, bytes.clone());
        output.events.push(SipEvent::Answered {
            id: id.into(),
            generation,
            session: invite.session.clone(),
        });
        self.incoming.insert(
            key,
            Incoming {
                invite,
                generation,
                bytes,
                state: State::AwaitingAck {
                    sequence,
                    retry: now.saturating_add(500),
                    interval: 500,
                    deadline: now.saturating_add(32_000),
                },
                local_cseq: 0,
                remote_cseq: sequence,
                local_media: local,
                answer_version: generation,
                offer,
                remote_target,
            },
        );
        Ok(output)
    }

    pub(crate) fn incoming_ack(
        &mut self,
        request: &Request,
        peer: SocketAddr,
    ) -> Result<(), SipError> {
        for cached in self.cache.values_mut() {
            if let Some(retry) = &cached.retry {
                if cached.peer == peer
                    && cached.request.call_id_header()?.value() == request.call_id_header()?.value()
                    && cached.request.cseq_header()?.typed()?.seq
                        == request.cseq_header()?.typed()?.seq
                    && cached.request.top_via_header()?.typed()?
                        == request.top_via_header()?.typed()?
                    && cached.request.from_header()?.typed()? == request.from_header()?.typed()?
                    && request.to_header()?.typed()?.tag().map(|tag| tag.value())
                        == Some(retry.tag.as_str())
                {
                    cached.retry = None;
                }
            }
        }
        for dialog in self.incoming.values_mut() {
            if dialog.matches(request, peer)?
                && matches!(dialog.state, State::AwaitingAck { sequence, .. } if request.cseq_header()?.typed()?.seq == sequence)
            {
                dialog.state = State::Established;
            }
        }
        Ok(())
    }

    pub(crate) fn incoming_request(
        &mut self,
        request: &Request,
        peer: SocketAddr,
        now: u64,
    ) -> Result<Option<DialogReply>, SipError> {
        if !matches!(request.method, Method::Bye | Method::Info | Method::Invite) {
            return Ok(None);
        }
        let retry_after = self.seed[0] % 11;
        let mut matched = None;
        for dialog in self.incoming.values_mut() {
            if dialog.matches(request, peer)? {
                matched = Some(dialog);
                break;
            }
        }
        let Some(dialog) = matched else {
            return Ok(None);
        };
        let sequence = request.cseq_header()?.typed()?.seq;
        if sequence <= dialog.remote_cseq {
            return Ok(Some(DialogReply::response(response(
                request,
                500,
                "Server Internal Error",
                "",
                "",
            )?)));
        }
        if request.method == Method::Invite {
            match dialog.state {
                State::Terminated { .. } => {
                    return Ok(Some(DialogReply::response(response(
                        request,
                        481,
                        "Call/Transaction Does Not Exist",
                        "",
                        "",
                    )?)))
                }
                State::AwaitingAck { .. } => {
                    return Ok(Some(DialogReply::response(response(
                        request,
                        500,
                        "Server Internal Error",
                        &format!("Retry-After: {retry_after}\r\n"),
                        "",
                    )?)))
                }
                State::Established => {}
            }
            let updated =
                match direction_update(&dialog.offer, &request.body, &dialog.invite.session) {
                    Ok(updated) => updated,
                    Err(_) => return Ok(Some(DialogReply::response(response(
                        request,
                        488,
                        "Not Acceptable Here",
                        "Warning: 399 rusterisk \"Only media direction changes are supported\"\r\n",
                        "",
                    )?))),
                };
            let target = request
                .headers
                .iter()
                .find(|header| header.name().eq_ignore_ascii_case("Contact"))
                .map(|header| rsipstack::sip::headers::typed::Contact::parse(header.value()))
                .transpose()?
                .map(|contact| contact.uri);
            let changed = updated.direction != dialog.invite.session.direction;
            let version = if changed {
                dialog
                    .answer_version
                    .checked_add(1)
                    .filter(|version| *version <= i64::MAX as u64)
                    .ok_or(SipError::Capacity)?
            } else {
                dialog.answer_version
            };
            let extra = format!("Contact: <sip:rusterisk@{}>\r\n", dialog.invite.local);
            let bytes = response(
                request,
                200,
                "OK",
                &extra,
                &updated.answer_with_version(dialog.local_media, dialog.generation, version),
            )?;
            let events = if changed {
                [SipEvent::PeerDirection {
                    id: dialog.invite.id.clone(),
                    generation: dialog.generation,
                    direction: updated.direction,
                }]
                .into_iter()
                .collect()
            } else {
                Vec::new()
            };
            dialog.invite.session = updated;
            dialog.offer = request.body.clone();
            dialog.answer_version = version;
            dialog.remote_cseq = sequence;
            if let Some(target) = target {
                dialog.remote_target = target;
            }
            dialog.bytes = bytes.clone();
            dialog.state = State::AwaitingAck {
                sequence,
                retry: now.saturating_add(500),
                interval: 500,
                deadline: now.saturating_add(32_000),
            };
            return Ok(Some(DialogReply::Accepted { bytes, events }));
        }
        if request.method == Method::Info {
            if matches!(dialog.state, State::Terminated { .. }) {
                return Ok(Some(DialogReply::response(response(
                    request,
                    481,
                    "Call/Transaction Does Not Exist",
                    "",
                    "",
                )?)));
            }
            dialog.remote_cseq = sequence;
            return crate::info::receive(request, &dialog.invite.id, dialog.generation).map(Some);
        }
        let events = match dialog.state {
            State::AwaitingAck { .. } | State::Established => [SipEvent::Ended {
                id: dialog.invite.id.clone(),
                generation: dialog.generation,
                cause: 16,
            }]
            .into_iter()
            .collect(),
            State::Terminated { .. } => Vec::new(),
        };
        dialog.remote_cseq = sequence;
        dialog.state = State::Terminated {
            expires: now.saturating_add(32_000),
        };
        Ok(Some(DialogReply::Accepted {
            bytes: response(request, 200, "OK", "", "")?,
            events,
        }))
    }

    pub(crate) fn incoming_session(&self, id: &str, generation: u64) -> Option<&Session> {
        self.incoming.values().find_map(|dialog| {
            if dialog.invite.id == id
                && dialog.generation == generation
                && matches!(dialog.state, State::AwaitingAck { .. } | State::Established)
            {
                Some(&dialog.invite.session)
            } else {
                None
            }
        })
    }

    pub(crate) fn hangup_incoming(
        &mut self,
        id: &str,
        generation: u64,
        now: u64,
    ) -> Result<Option<Output>, SipError> {
        let Some(dialog) = self
            .incoming
            .values_mut()
            .find(|dialog| dialog.invite.id == id && dialog.generation == generation)
        else {
            return Ok(None);
        };
        match dialog.state {
            State::AwaitingAck { .. } | State::Established => {
                Ok(Some(Output::response(dialog.invite.peer, dialog.bye(now)?)))
            }
            State::Terminated { .. } => Ok(Some(Output {
                datagrams: Vec::new(),
                events: Vec::new(),
            })),
        }
    }

    pub(crate) fn incoming_tick(&mut self, now: u64) -> Result<Output, SipError> {
        self.cache
            .retain(|_, cached| cached.expires.is_none_or(|expires| expires > now));
        self.incoming.retain(
            |_, dialog| !matches!(dialog.state, State::Terminated { expires } if expires <= now),
        );
        let mut output = Output {
            datagrams: Vec::new(),
            events: Vec::new(),
        };
        for cached in self.cache.values_mut() {
            if let Some(retry) = &mut cached.retry {
                if retry.next <= now {
                    output.datagrams.push(Datagram {
                        peer: cached.peer,
                        bytes: cached.bytes.clone(),
                    });
                    retry.interval = (retry.interval * 2).min(4000);
                    retry.next = now.saturating_add(retry.interval);
                }
            }
        }
        for dialog in self.incoming.values_mut() {
            match &mut dialog.state {
                State::AwaitingAck { deadline, .. } if *deadline <= now => {
                    output.events.push(SipEvent::Ended {
                        id: dialog.invite.id.clone(),
                        generation: dialog.generation,
                        cause: 102,
                    });
                    output.datagrams.push(Datagram {
                        peer: dialog.invite.peer,
                        bytes: dialog.bye(now)?,
                    });
                }
                State::AwaitingAck {
                    retry, interval, ..
                } if *retry <= now => {
                    output.datagrams.push(Datagram {
                        peer: dialog.invite.peer,
                        bytes: dialog.bytes.clone(),
                    });
                    *interval = (*interval * 2).min(4000);
                    *retry = now.saturating_add(*interval);
                }
                State::AwaitingAck { .. } | State::Established | State::Terminated { .. } => {}
            }
        }
        Ok(output)
    }

    pub fn reject(&mut self, id: &str, reason: RejectReason, now: u64) -> Result<Output, SipError> {
        let key = self
            .pending
            .iter()
            .find(|(_, invite)| invite.id == id)
            .map(|(key, _)| key.clone())
            .ok_or(SipError::InvalidPacket)?;
        let invite = self.pending.remove(&key).ok_or(SipError::InvalidPacket)?;
        let (code, phrase) = reason.response();
        let request = tagged(&invite.request, &invite.local_tag)?;
        let bytes = response(&request, code, phrase, "", "")?;
        self.update_invite_reply(&invite, &bytes, now)?;
        Ok(Output::response(invite.peer, bytes))
    }
}
