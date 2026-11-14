use crate::{
    response, Datagram, DialogReply, Engine, Output, PendingInvite, Session, SipError, SipEvent,
};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method, Request};
use std::net::SocketAddr;

enum State {
    AwaitingAck {
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
}

impl Incoming {
    fn matches(&self, request: &Request, peer: SocketAddr) -> Result<bool, SipError> {
        Ok(self.invite.peer == peer
            && request.from_header()?.typed()?.tag()
                == self.invite.request.from_header()?.typed()?.tag()
            && request.to_header()?.typed()?.tag().map(|tag| tag.value())
                == Some(self.invite.local_tag.as_str()))
    }

    fn bye(&mut self, now: u64) -> Result<Vec<u8>, SipError> {
        self.local_cseq = self.local_cseq.checked_add(1).ok_or(SipError::Capacity)?;
        let invite = tagged(&self.invite.request, &self.invite.local_tag)?;
        let target = invite.contact_header()?.typed()?.uri;
        let packet = format!("BYE {target} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-{}-BYE-{};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {} BYE\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", self.invite.local, self.invite.local_tag, self.local_cseq, invite.to_header()?.value(), invite.from_header()?.value(), invite.call_id_header()?.value(), self.local_cseq).into_bytes();
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

fn tagged(request: &Request, tag: &str) -> Result<Request, SipError> {
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
        && invite.request.uri == request.uri
        && invite.request.cseq_header()?.typed()?.seq == request.cseq_header()?.typed()?.seq
        && invite.request.top_via_header()?.typed()? == request.top_via_header()?.typed()?
        && invite.request.from_header()?.typed()? == request.from_header()?.typed()?
        && invite.request.to_header()?.typed()? == request.to_header()?.typed()?)
}

impl Engine {
    pub(crate) fn incoming_cancel(
        &mut self,
        key: String,
        request: Request,
        peer: SocketAddr,
        now: u64,
    ) -> Result<Output, SipError> {
        let call_id = request.call_id_header()?.value().to_owned();
        let invite = self
            .pending
            .get(&call_id)
            .or_else(|| self.incoming.get(&call_id).map(|dialog| &dialog.invite));
        let Some(invite) = invite else {
            return Ok(Output::response(
                peer,
                response(&request, 481, "Call/Transaction Does Not Exist", "", "")?,
            ));
        };
        if !same_invite(invite, &request, peer)? {
            return Ok(Output::response(
                peer,
                response(&request, 481, "Call/Transaction Does Not Exist", "", "")?,
            ));
        }
        let bytes = response(&tagged(&request, &invite.local_tag)?, 200, "OK", "", "")?;
        let mut output = self.cache_reply(key, request, peer, now, bytes, Vec::new())?;
        if let Some(invite) = self.pending.remove(&call_id) {
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
                    retry: now.saturating_add(500),
                    interval: 500,
                    deadline: now.saturating_add(32_000),
                },
                local_cseq: 0,
                remote_cseq: sequence,
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
        if let Some(dialog) = self.incoming.get_mut(request.call_id_header()?.value()) {
            if dialog.matches(request, peer)?
                && request.cseq_header()?.typed()?.seq
                    == dialog.invite.request.cseq_header()?.typed()?.seq
                && matches!(dialog.state, State::AwaitingAck { .. })
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
        if request.method != Method::Bye {
            return Ok(None);
        }
        let Some(dialog) = self.incoming.get_mut(request.call_id_header()?.value()) else {
            return Ok(None);
        };
        if !dialog.matches(request, peer)? {
            return Ok(Some(DialogReply::Rejected(response(
                request,
                481,
                "Call/Transaction Does Not Exist",
                "",
                "",
            )?)));
        }
        let sequence = request.cseq_header()?.typed()?.seq;
        if sequence <= dialog.remote_cseq {
            return Ok(Some(DialogReply::Rejected(response(
                request,
                500,
                "Server Internal Error",
                "",
                "",
            )?)));
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
