use crate::{
    dialog_target::DialogTarget, transport::UriSecurity, Datagram, DialogReply, Disconnection,
    Engine, Output, Session, SessionSecurity, SignalingRoute, SipError, SipEvent, StreamFlowId,
};
use rsipstack::sip::{
    headers::untyped::ToTypedHeader, Header, HeadersExt, Method, Request, Response, SipMessage,
};
use rusterisk_config::{Auth, Codec, Protocol};
use std::net::SocketAddr;
mod forked;
mod profile;
pub use forked::ForkTeardownFailure;
pub(crate) use forked::Forks;
pub use profile::{OutboundProfile, PreparedOutbound};
use profile::{RouteSelection, SelectedOutbound};

pub struct Outbound {
    pub id: String,
    pub generation: u64,
    pub endpoint: String,
    pub caller: String,
    pub local_rtp: SocketAddr,
    pub timeout_ms: Option<u64>,
}

struct ValidatedOutbound {
    outbound: Outbound,
    deadline: Option<u64>,
    transaction_deadline: u64,
    now: u64,
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
    Established(DialogMedia),
    Cancelling {
        expires: u64,
    },
    Terminated {
        expires: u64,
    },
    RejectedTarget {
        expires: u64,
    },
}

pub(crate) struct Outgoing {
    id: String,
    generation: u64,
    route: SignalingRoute,
    reliable: bool,
    protocol: Protocol,
    uri_security: UriSecurity,
    invite: Request,
    bytes: Vec<u8>,
    remote_to: String,
    remote_contact: DialogTarget,
    codecs: Vec<Codec>,
    deadline: Option<u64>,
    transaction_deadline: u64,
    state: State,
    cseq: u32,
    remote_cseq: Option<u32>,
    credentials: Vec<Auth>,
    origin_encoding: crate::digest::CredentialEncoding,
    proxy_encoding: crate::digest::CredentialEncoding,
    challenges: Vec<AcknowledgedResponse>,
    final_response: FinalResponse,
    local_media: SocketAddr,
    local_security: SessionSecurity,
    session_id: u64,
    authority: crate::ReferAuthority,
}

struct DialogMedia {
    session: Session,
    peer_description: Vec<u8>,
    answer_version: u64,
    pending: Option<InviteAnswer>,
}

impl Engine {
    pub(crate) fn outgoing_replacement(
        &self,
        selector: &crate::Replaces,
        selected: &mut crate::transfer::replacement::Selection,
    ) -> Result<(), SipError> {
        use crate::transfer::replacement::Eligibility;
        for dialog in self.outgoing.values() {
            let from = dialog.invite.from_header()?.typed()?;
            let to = rsipstack::sip::headers::typed::To::parse(&dialog.remote_to)?;
            if dialog.invite.call_id_header()?.value() != selector.call_id()
                || from.tag().map(|tag| tag.value()) != Some(selector.to_tag())
                || to.tag().map(|tag| tag.value()) != Some(selector.from_tag())
            {
                continue;
            }
            let eligibility = match dialog.state {
                State::Established(_) => match dialog.uri_security {
                    UriSecurity::Sip => Eligibility::Confirmed,
                    UriSecurity::Sips => Eligibility::Unsupported,
                },
                State::Calling { .. } | State::Proceeding => Eligibility::Unsupported,
                State::CancelPending { .. }
                | State::Cancelling { .. }
                | State::Terminated { .. }
                | State::RejectedTarget { .. } => Eligibility::Ended,
            };
            selected.include(
                selector,
                &dialog.id,
                dialog.generation,
                &dialog.authority,
                eligibility,
            );
        }
        Ok(())
    }

    pub(crate) fn outgoing_refer_dialog(
        &mut self,
        request: &Request,
        route: SignalingRoute,
    ) -> Result<crate::transfer::dialog::Dialog, crate::transfer::dialog::MatchError> {
        use crate::transfer::dialog::{Dialog, MatchError, Usage};
        for dialog in self.outgoing.values_mut() {
            if !dialog.matches(request, route)? {
                continue;
            }
            let sequence = request.cseq_header()?.typed()?.seq;
            if dialog.remote_cseq.is_some_and(|last| sequence <= last) {
                return Err(MatchError::Rejected(500, "Server Internal Error"));
            }
            dialog.remote_cseq = Some(sequence);
            return Ok(Dialog {
                id: dialog.id.clone(),
                generation: dialog.generation,
                route: dialog.route,
                wire: crate::transfer::wire::Dialog::remote(request)?,
                contact: dialog.invite.contact_header()?.value().into(),
                authority: dialog.authority.clone(),
                security: dialog.uri_security,
                usage: match dialog.state {
                    State::Established(_) => Usage::Active,
                    State::Calling { .. }
                    | State::Proceeding
                    | State::CancelPending { .. }
                    | State::Cancelling { .. }
                    | State::Terminated { .. }
                    | State::RejectedTarget { .. } => Usage::Ended,
                },
            });
        }
        Err(MatchError::Missing)
    }

    pub(crate) fn outgoing_notify(
        &mut self,
        id: &str,
        generation: u64,
        contact: &str,
        notification: crate::transfer::notify::Notification,
        target: Option<&crate::transfer::dialog::Target>,
    ) -> Result<Option<Datagram>, SipError> {
        let Some(dialog) = self
            .outgoing
            .values_mut()
            .find(|dialog| dialog.id == id && dialog.generation == generation)
        else {
            return Ok(None);
        };
        let target = target
            .map(|target| {
                DialogTarget::from_uri(dialog.protocol, dialog.uri_security, target.uri())
            })
            .transpose()?;
        dialog.cseq = dialog
            .cseq
            .checked_add(1)
            .filter(|sequence| *sequence < 1 << 31)
            .ok_or(SipError::Capacity)?;
        let packet = related(dialog, Method::Notify, &dialog.remote_to, dialog.cseq, true)?;
        let mut request = request(std::str::from_utf8(&packet)?)?;
        if let Some(target) = &target {
            request.uri = target.uri().clone();
        }
        let bytes = crate::transfer::notify::encode(request, contact, notification)?;
        if let Some(target) = target {
            dialog.remote_contact = target;
        }
        Ok(Some(Datagram::on_route(dialog.route, bytes)))
    }

    pub(super) fn outgoing_disconnected(
        &mut self,
        closed: Disconnection,
        events: &mut Vec<SipEvent>,
    ) {
        self.outgoing.retain(|_, dialog| {
            if !closed.matches(dialog.route) {
                return true;
            }
            match dialog.state {
                State::Calling { .. } | State::Proceeding | State::Established(_) => {
                    events.push(SipEvent::Ended {
                        id: dialog.id.clone(),
                        generation: dialog.generation,
                        cause: 41,
                    })
                }
                State::CancelPending { .. }
                | State::Cancelling { .. }
                | State::Terminated { .. }
                | State::RejectedTarget { .. } => {}
            }
            false
        });
    }
}

struct InviteAnswer {
    sequence: u32,
    bytes: Vec<u8>,
    retry: u64,
    interval: u64,
    deadline: u64,
}

impl Outgoing {
    fn answer(&mut self, source: &[u8]) -> Result<DialogMedia, SipError> {
        let session = Session::parse(std::str::from_utf8(source)?, &self.codecs)?;
        match (&self.local_security, &session.security) {
            (SessionSecurity::Rtp, SessionSecurity::Rtp) => {}
            (SessionSecurity::WebRtc(_), SessionSecurity::WebRtc(description)) => {
                if description.setup() == crate::DtlsSetup::ActPass {
                    return Err(SipError::Unsupported);
                }
            }
            (SessionSecurity::Rtp, SessionSecurity::WebRtc(_))
            | (SessionSecurity::WebRtc(_), SessionSecurity::Rtp) => {
                return Err(SipError::Unsupported)
            }
        }
        let local_security =
            crate::media_direction::establish(&self.local_security, &session.security)?;
        let answer_version = crate::media_direction::version(&local_security, self.session_id)?;
        self.local_security = local_security;
        Ok(DialogMedia {
            session,
            answer_version,
            peer_description: source.to_vec(),
            pending: None,
        })
    }

    fn matches(&self, request: &Request, route: SignalingRoute) -> Result<bool, SipError> {
        let remote = rsipstack::sip::headers::typed::To::parse(&self.remote_to)?;
        Ok(self.route == route
            && remote.tag().is_some()
            && request.call_id_header()?.value() == self.invite.call_id_header()?.value()
            && request.from_header()?.typed()?.tag() == remote.tag()
            && request.to_header()?.typed()?.tag() == self.invite.from_header()?.typed()?.tag())
    }
}

struct AcknowledgedResponse {
    response: Response,
    ack: Vec<u8>,
    expires: u64,
}

enum FinalResponse {
    Pending,
    Acknowledged(AcknowledgedResponse),
    Expired,
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
    let authorization_name = if proxy {
        "Proxy-Authorization"
    } else {
        "Authorization"
    };
    let previous = dialog
        .invite
        .headers
        .iter()
        .find(|header| header.name().eq_ignore_ascii_case(authorization_name))
        .map(|header| header.value());
    let selected = match crate::digest::Header::choose_challenge(
        response
            .headers
            .iter()
            .filter(|header| header.name().eq_ignore_ascii_case(name))
            .map(|header| header.value()),
        &dialog.credentials,
        previous,
    ) {
        Ok(Some(selected)) => selected,
        Ok(None)
        | Err(crate::digest::DigestError::Invalid | crate::digest::DigestError::Unsupported) => {
            return Ok(None)
        }
    };
    let mut entropy = [0; 16];
    getrandom::fill(&mut entropy).map_err(|_| SipError::Entropy)?;
    let nonce = format!("{:032x}", u128::from_be_bytes(entropy));
    let authorization = match selected.authorization(
        "INVITE",
        &dialog.invite.uri.to_string(),
        &dialog.invite.body,
        &nonce,
        1,
    ) {
        Ok(authorization) => authorization,
        Err(crate::digest::DigestError::Invalid | crate::digest::DigestError::Unsupported) => {
            return Ok(None)
        }
    };
    let encoding = selected.encoding();
    let mut invite = dialog.invite.clone();
    let other_name = if proxy {
        "Authorization"
    } else {
        "Proxy-Authorization"
    };
    let uri = invite.uri.to_string();
    if let Some(header) = invite
        .headers
        .iter_mut()
        .find(|header| header.name().eq_ignore_ascii_case(other_name))
    {
        let renewed = crate::digest::Header::parse(header.value()).and_then(|previous| {
            previous.renew_authorization(
                &dialog.credentials,
                "INVITE",
                &uri,
                &invite.body,
                if proxy {
                    dialog.origin_encoding
                } else {
                    dialog.proxy_encoding
                },
            )
        });
        let renewed = match renewed {
            Ok(Some(renewed)) => renewed,
            Ok(None)
            | Err(crate::digest::DigestError::Invalid | crate::digest::DigestError::Unsupported) => {
                return Ok(None);
            }
        };
        *header = if proxy {
            Header::Authorization(rsipstack::sip::headers::untyped::Authorization::new(
                renewed,
            ))
        } else {
            Header::ProxyAuthorization(rsipstack::sip::headers::untyped::ProxyAuthorization::new(
                renewed,
            ))
        };
    }
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
            authorization,
        ))
    } else {
        Header::Authorization(rsipstack::sip::headers::untyped::Authorization::new(
            authorization,
        ))
    };
    invite.headers.unique_push(header);
    let bytes = invite.to_string().into_bytes();
    if bytes.len() > crate::MAX_MESSAGE {
        return Err(SipError::Capacity);
    }
    dialog.cseq = sequence;
    dialog.invite = invite;
    if proxy {
        dialog.proxy_encoding = encoding;
    } else {
        dialog.origin_encoding = encoding;
    }
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
    related_to(
        dialog,
        method,
        to,
        cseq,
        fresh_branch,
        &dialog.remote_contact,
    )
}

fn related_to(
    dialog: &Outgoing,
    method: Method,
    to: &str,
    cseq: u32,
    fresh_branch: bool,
    target: &DialogTarget,
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
        target.uri()
    };
    let bytes = format!(
        "{method} {uri} SIP/2.0\r\nVia: {via}\r\nFrom: {}\r\nTo: {to}\r\nCall-ID: {}\r\nCSeq: {cseq} {method}\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n",
        dialog.invite.from_header()?.value(),
        dialog.invite.call_id_header()?.value()
    );
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
    let mut body = format!(
        "v=0\r\no=rusterisk {serial} {serial} IN {family} {}\r\ns=Rusterisk\r\nc=IN {family} {}\r\nt=0 0\r\nm=audio {} RTP/AVP {payloads} {events}\r\n",
        local.ip(),
        local.ip(),
        local.port()
    );
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
        Ok(Some(Datagram::on_route(dialog.route, bytes)))
    }

    pub fn originate(&mut self, outbound: Outbound, now: u64) -> Result<Output, SipError> {
        self.originate_inner(outbound, None, None, now)
    }

    pub fn originate_to(
        &mut self,
        outbound: Outbound,
        peer: SocketAddr,
        now: u64,
    ) -> Result<Output, SipError> {
        self.originate_inner(outbound, Some(peer), None, now)
    }

    pub fn originate_with_sdp(
        &mut self,
        outbound: Outbound,
        description: &crate::WebRtcDescription,
        now: u64,
    ) -> Result<Output, SipError> {
        self.originate_inner(outbound, None, Some(description.as_str()), now)
    }

    pub fn originate_to_with_sdp(
        &mut self,
        outbound: Outbound,
        peer: SocketAddr,
        description: &crate::WebRtcDescription,
        now: u64,
    ) -> Result<Output, SipError> {
        self.originate_inner(outbound, Some(peer), Some(description.as_str()), now)
    }

    pub(crate) fn originate_inner(
        &mut self,
        outbound: Outbound,
        resolved_peer: Option<SocketAddr>,
        sdp: Option<&str>,
        now: u64,
    ) -> Result<Output, SipError> {
        let request = self.validate_outbound(outbound, now)?;
        let profile = OutboundProfile::capture(&self.config, &request.outbound.endpoint)?;
        let selection = match resolved_peer {
            Some(_) => RouteSelection::Static,
            None => RouteSelection::RegisteredFirst,
        };
        let selected = profile.select(&self.contacts, selection, now)?;
        self.originate_selected(request, selected, resolved_peer, sdp, None)
    }

    pub fn prepare_outbound(
        &self,
        profile: OutboundProfile,
        now: u64,
    ) -> Result<PreparedOutbound, SipError> {
        profile
            .select(&self.contacts, RouteSelection::RegisteredFirst, now)?
            .prepare()
    }

    pub fn originate_prepared(
        &mut self,
        outbound: Outbound,
        prepared: PreparedOutbound,
        resolved_peer: Option<SocketAddr>,
        description: Option<&crate::WebRtcDescription>,
        now: u64,
    ) -> Result<Output, SipError> {
        self.originate_prepared_inner(outbound, prepared, resolved_peer, description, None, now)
    }

    pub fn originate_prepared_on_flow(
        &mut self,
        outbound: Outbound,
        prepared: PreparedOutbound,
        resolved_peer: Option<SocketAddr>,
        description: Option<&crate::WebRtcDescription>,
        flow: StreamFlowId,
        now: u64,
    ) -> Result<Output, SipError> {
        if self.protocol == Protocol::Udp {
            return Err(SipError::Unsupported);
        }
        self.originate_prepared_inner(
            outbound,
            prepared,
            resolved_peer,
            description,
            Some(flow),
            now,
        )
    }

    fn originate_prepared_inner(
        &mut self,
        outbound: Outbound,
        prepared: PreparedOutbound,
        resolved_peer: Option<SocketAddr>,
        description: Option<&crate::WebRtcDescription>,
        flow: Option<StreamFlowId>,
        now: u64,
    ) -> Result<Output, SipError> {
        let request = self.validate_outbound(outbound, now)?;
        self.originate_selected(
            request,
            prepared.into_selected(),
            resolved_peer,
            description.map(crate::WebRtcDescription::as_str),
            flow,
        )
    }

    fn validate_outbound(
        &self,
        outbound: Outbound,
        now: u64,
    ) -> Result<ValidatedOutbound, SipError> {
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
        Ok(ValidatedOutbound {
            outbound,
            deadline,
            transaction_deadline,
            now,
        })
    }

    fn originate_selected(
        &mut self,
        validated: ValidatedOutbound,
        selected: SelectedOutbound,
        resolved_peer: Option<SocketAddr>,
        sdp: Option<&str>,
        flow: Option<StreamFlowId>,
    ) -> Result<Output, SipError> {
        let ValidatedOutbound {
            outbound,
            deadline,
            transaction_deadline,
            now,
        } = validated;
        if outbound.endpoint != selected.profile.endpoint.name {
            return Err(SipError::InvalidPacket);
        }
        let route = selected.route(&self.contacts, resolved_peer, flow, now)?;
        let profile = selected.profile;
        let endpoint = profile.endpoint;
        if endpoint.webrtc != sdp.is_some() {
            return Err(SipError::Unsupported);
        }
        let local_security = if let Some(sdp) = sdp {
            let description = crate::WebRtcDescription::parse(sdp)?;
            if description.setup() != crate::DtlsSetup::ActPass {
                return Err(SipError::Unsupported);
            }
            Session::parse(description.as_str(), &endpoint.codecs)?;
            SessionSecurity::WebRtc(description)
        } else {
            SessionSecurity::Rtp
        };
        let transport = profile.transport;
        let uri = selected.uri;
        let signaling = transport.bind;
        let protocol = transport.protocol;
        let uri_security = selected.security;
        let token = self.token()?;
        let body = match sdp {
            Some(sdp) => sdp.to_owned(),
            None => formats(&endpoint.codecs, outbound.local_rtp, self.serial),
        };
        let caller = if outbound.caller.is_empty() {
            "rusterisk"
        } else {
            &outbound.caller
        };
        let token_transport = crate::transport::token(protocol);
        let contact = crate::transport::contact(protocol, signaling, uri_security);
        let bytes = format!(
            "INVITE {uri} SIP/2.0\r\nVia: SIP/2.0/{token_transport} {signaling};branch=z9hG4bK-{token};rport\r\nFrom: <sip:{caller}@{}>;tag={token}\r\nTo: <{uri}>\r\nCall-ID: {token}@rusterisk\r\nCSeq: 1 INVITE\r\nContact: {contact}\r\nMax-Forwards: 70\r\nAllow: INVITE, ACK, CANCEL, BYE, OPTIONS, INFO, REFER\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",
            signaling.ip(),
            body.len()
        );
        let invite = request(&bytes)?;
        let call_id = invite.call_id_header()?.value().to_owned();
        let dialog = Outgoing {
            authority: profile.authority,
            id: outbound.id,
            generation: outbound.generation,
            route,
            reliable: protocol != rusterisk_config::Protocol::Udp,
            protocol,
            uri_security,
            remote_to: invite.to_header()?.value().into(),
            remote_contact: DialogTarget::from_uri(protocol, uri_security, &uri)?,
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
            credentials: profile.credentials,
            challenges: Vec::new(),
            origin_encoding: crate::digest::CredentialEncoding::Unspecified,
            proxy_encoding: crate::digest::CredentialEncoding::Unspecified,
            final_response: FinalResponse::Pending,
            local_media: outbound.local_rtp,
            local_security,
            session_id: self.serial,
        };
        self.outgoing.insert(call_id, dialog);
        Ok(Output::response(route, bytes.into_bytes()))
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
            State::CancelPending { .. }
            | State::Cancelling { .. }
            | State::Terminated { .. }
            | State::RejectedTarget { .. } => {
                return Ok(Output {
                    datagrams: Vec::new(),
                    events: Vec::new(),
                });
            }
        };
        Ok(Output::response(dialog.route, packet))
    }

    pub fn tick(&mut self, now: u64) -> Result<Output, SipError> {
        let mut output = self.tick_inner(now)?;
        self.expire_contacts(now, &mut output.events);
        self.track_output(&output, now)?;
        output.datagrams.extend(self.transactions.tick(now));
        Ok(output)
    }

    fn tick_inner(&mut self, now: u64) -> Result<Output, SipError> {
        self.forks.expire(now)?;
        let expired = self.transactions.expire(now);
        let mut output = Output {
            datagrams: Vec::new(),
            events: Vec::new(),
        };
        for receipt in expired {
            let closed = self.refer_receipt(receipt);
            output.events.extend(closed.events);
        }
        let refers = self.refer_tick(now)?;
        output.datagrams.extend(refers.datagrams);
        output.events.extend(refers.events);
        let incoming = self.incoming_tick(now)?;
        output.datagrams.extend(incoming.datagrams);
        output.events.extend(incoming.events);
        self.outgoing.retain(|_, dialog| !matches!(dialog.state, State::CancelPending { expires, .. } | State::Cancelling { expires } | State::Terminated { expires } | State::RejectedTarget { expires } if expires <= now) || self.refers.holds(&dialog.id, dialog.generation));
        for dialog in self.outgoing.values_mut() {
            dialog
                .challenges
                .retain(|challenge| challenge.expires > now);
            if matches!(&dialog.final_response, FinalResponse::Acknowledged(response) if response.expires <= now)
            {
                dialog.final_response = FinalResponse::Expired;
            }
            if let State::Established(media) = &mut dialog.state {
                if media
                    .pending
                    .as_ref()
                    .is_some_and(|answer| answer.deadline <= now)
                {
                    dialog.cseq = dialog.cseq.checked_add(1).ok_or(SipError::Capacity)?;
                    output.datagrams.push(Datagram::on_route(
                        dialog.route,
                        related(dialog, Method::Bye, &dialog.remote_to, dialog.cseq, true)?,
                    ));
                    output.events.push(SipEvent::Ended {
                        id: dialog.id.clone(),
                        generation: dialog.generation,
                        cause: 102,
                    });
                    dialog.state = State::Terminated {
                        expires: now.saturating_add(32_000),
                    };
                    continue;
                }
                if let Some(answer) = &mut media.pending {
                    if answer.retry <= now {
                        output
                            .datagrams
                            .push(Datagram::on_route(dialog.route, answer.bytes.clone()));
                        answer.interval = (answer.interval * 2).min(4000);
                        answer.retry = now.saturating_add(answer.interval);
                    }
                }
            }
            match &mut dialog.state {
                State::Calling { retry, interval }
                    if dialog
                        .deadline
                        .map(|deadline| deadline.min(dialog.transaction_deadline))
                        .unwrap_or(dialog.transaction_deadline)
                        > now =>
                {
                    if *retry <= now && !dialog.reliable {
                        output
                            .datagrams
                            .push(Datagram::on_route(dialog.route, dialog.bytes.clone()));
                        *interval = (*interval * 2).min(32_000);
                        *retry = now.saturating_add(*interval);
                    }
                }
                State::CancelPending {
                    retry, interval, ..
                } => {
                    if *retry <= now && !dialog.reliable {
                        output
                            .datagrams
                            .push(Datagram::on_route(dialog.route, dialog.bytes.clone()));
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
                    output.datagrams.push(Datagram::on_route(
                        dialog.route,
                        related(
                            dialog,
                            Method::Cancel,
                            dialog.invite.to_header()?.value(),
                            dialog.cseq,
                            false,
                        )?,
                    ));
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
                | State::Terminated { .. }
                | State::RejectedTarget { .. } => {}
            }
        }
        Ok(output)
    }

    pub(crate) fn receive_response(
        &mut self,
        response: Response,
        route: SignalingRoute,
        now: u64,
    ) -> Result<Output, SipError> {
        let mut output = Output {
            datagrams: Vec::new(),
            events: Vec::new(),
        };
        let Some(dialog) = self.outgoing.get_mut(response.call_id_header()?.value()) else {
            return Ok(output);
        };
        if dialog.route == route {
            if let Some(challenge) = dialog
                .challenges
                .iter()
                .find(|challenge| challenge.expires > now && challenge.response == response)
            {
                return Ok(Output::response(route, challenge.ack.clone()));
            }
        }
        if dialog.route != route
            || response.cseq_header()?.value() != dialog.invite.cseq_header()?.value()
            || response.transaction_id()? != dialog.invite.transaction_id()?
            || response.from_header()?.value() != dialog.invite.from_header()?.value()
        {
            return Ok(output);
        }
        if matches!(dialog.state, State::RejectedTarget { .. }) {
            return Ok(output);
        }
        match &dialog.final_response {
            FinalResponse::Acknowledged(previous) if previous.expires > now => {
                if previous.response.status_code == response.status_code
                    && previous.response.to_header()?.value() == response.to_header()?.value()
                {
                    output
                        .datagrams
                        .push(Datagram::on_route(route, previous.ack.clone()));
                } else if (200..300).contains(&previous.response.status_code.code())
                    && (200..300).contains(&response.status_code.code())
                    && previous.response.to_header()?.typed()?.tag()
                        != response.to_header()?.typed()?.tag()
                {
                    return forked::receive(
                        dialog,
                        &mut self.forks,
                        &mut self.transactions,
                        &response,
                        previous.expires,
                        now,
                    );
                }
                return Ok(output);
            }
            FinalResponse::Acknowledged(_) | FinalResponse::Expired => {
                dialog.final_response = FinalResponse::Expired;
                return Ok(output);
            }
            FinalResponse::Pending => {}
        }
        let code = response.status_code.code();
        if code < 200 {
            if let State::CancelPending { expires, .. } = dialog.state {
                output.datagrams.push(Datagram::on_route(
                    route,
                    related(
                        dialog,
                        Method::Cancel,
                        dialog.invite.to_header()?.value(),
                        dialog.cseq,
                        false,
                    )?,
                ));
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
        let remote_to = response.to_header()?.value().to_owned();
        let target = if code < 300 {
            let target = match DialogTarget::from_response(
                dialog.protocol,
                dialog.uri_security,
                &response,
            ) {
                Ok(Some(target)) => target,
                Ok(None) | Err(_) => {
                    dialog.state = State::RejectedTarget {
                        expires: now.saturating_add(32_000),
                    };
                    output.events.push(SipEvent::Ended {
                        id: dialog.id.clone(),
                        generation: dialog.generation,
                        cause: 21,
                    });
                    return Ok(output);
                }
            };
            Some(target)
        } else {
            None
        };
        let ack = related_to(
            dialog,
            Method::Ack,
            &remote_to,
            dialog.invite.cseq_header()?.typed()?.seq,
            code < 300,
            target.as_ref().unwrap_or(&dialog.remote_contact),
        )?;
        if let Some(target) = target {
            dialog.remote_contact = target;
        }
        dialog.remote_to = remote_to;
        output
            .datagrams
            .push(Datagram::on_route(route, ack.clone()));
        if let Some(bytes) = authenticate(dialog, &response, now)? {
            dialog.challenges.push(AcknowledgedResponse {
                response,
                ack,
                expires: now.saturating_add(32_000),
            });
            output.datagrams.push(Datagram::on_route(route, bytes));
            return Ok(output);
        }
        dialog.final_response = FinalResponse::Acknowledged(AcknowledgedResponse {
            response: response.clone(),
            ack,
            expires: now.saturating_add(32_000),
        });
        if code < 300 {
            match dialog.state {
                State::Calling { .. } | State::Proceeding => {
                    let media = match dialog.answer(&response.body) {
                        Ok(media) => media,
                        Err(_) => {
                            dialog.cseq = dialog.cseq.checked_add(1).ok_or(SipError::Capacity)?;
                            output.datagrams.push(Datagram::on_route(
                                route,
                                related(dialog, Method::Bye, &dialog.remote_to, dialog.cseq, true)?,
                            ));
                            output.events.push(SipEvent::Ended {
                                id: dialog.id.clone(),
                                generation: dialog.generation,
                                cause: 21,
                            });
                            dialog.state = State::Terminated {
                                expires: now.saturating_add(32_000),
                            };
                            return Ok(output);
                        }
                    };
                    output.events.push(SipEvent::Answered {
                        id: dialog.id.clone(),
                        generation: dialog.generation,
                        session: media.session.clone(),
                    });
                    dialog.state = State::Established(media);
                }
                State::CancelPending { .. } | State::Cancelling { .. } => {
                    dialog.cseq = dialog.cseq.checked_add(1).ok_or(SipError::Capacity)?;
                    output.datagrams.push(Datagram::on_route(
                        route,
                        related(dialog, Method::Bye, &dialog.remote_to, dialog.cseq, true)?,
                    ));
                    dialog.state = State::Terminated {
                        expires: now.saturating_add(32_000),
                    };
                }
                State::Established(_) | State::Terminated { .. } | State::RejectedTarget { .. } => {
                }
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
                if let State::Established(media) = &dialog.state {
                    return Some(&media.session);
                }
            }
            None
        })
    }

    pub(crate) fn outgoing_request(
        &mut self,
        request: &Request,
        route: SignalingRoute,
        now: u64,
    ) -> Result<Option<DialogReply>, SipError> {
        if !matches!(request.method, Method::Bye | Method::Info | Method::Invite) {
            return Ok(None);
        }
        let Some(dialog) = self.outgoing.get_mut(request.call_id_header()?.value()) else {
            return Ok(None);
        };
        if !dialog.matches(request, route)? {
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
            let target =
                match DialogTarget::from_request(dialog.protocol, dialog.uri_security, request) {
                    Ok(target) => target,
                    Err(_) => {
                        return Ok(Some(DialogReply::Rejected(crate::response(
                            request,
                            400,
                            "Bad Request",
                            "",
                            "",
                        )?)));
                    }
                };
            let State::Established(media) = &mut dialog.state else {
                return Ok(Some(DialogReply::response(crate::response(
                    request,
                    481,
                    "Call/Transaction Does Not Exist",
                    "",
                    "",
                )?)));
            };
            if media.pending.is_some() {
                return Ok(Some(DialogReply::response(crate::response(
                    request,
                    500,
                    "Server Internal Error",
                    &format!("Retry-After: {}\r\n", self.seed[0] % 11),
                    "",
                )?)));
            }
            let updated = match crate::media_direction::update(
                &media.peer_description,
                &request.body,
                &media.session,
                &dialog.local_security,
            ) {
                Ok(updated) => updated,
                Err(_) => {
                    return Ok(Some(DialogReply::response(crate::response(
                        request,
                        488,
                        "Not Acceptable Here",
                        "Warning: 399 rusterisk \"Only media direction changes are supported\"\r\n",
                        "",
                    )?)));
                }
            };
            let version = media
                .answer_version
                .checked_add(1)
                .filter(|version| *version <= i64::MAX as u64)
                .ok_or(SipError::Capacity)?;
            let extra = format!("Contact: {}\r\n", dialog.invite.contact_header()?.value());
            let bytes = crate::response(
                request,
                200,
                "OK",
                &extra,
                &crate::media_direction::answer(
                    &updated,
                    &dialog.local_security,
                    dialog.local_media,
                    dialog.session_id,
                    version,
                )?,
            )?;
            let events = if updated.direction != media.session.direction {
                [SipEvent::PeerDirection {
                    id: dialog.id.clone(),
                    generation: dialog.generation,
                    direction: updated.direction,
                }]
                .into_iter()
                .collect()
            } else {
                Vec::new()
            };
            media.session = updated;
            media.peer_description = request.body.clone();
            media.answer_version = version;
            media.pending = Some(InviteAnswer {
                sequence,
                bytes: bytes.clone(),
                retry: now.saturating_add(500),
                interval: 500,
                deadline: now.saturating_add(32_000),
            });
            dialog.remote_cseq = Some(sequence);
            if let Some(target) = target {
                dialog.remote_contact = target;
            }
            return Ok(Some(DialogReply::Accepted { bytes, events }));
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
            | State::Terminated { .. }
            | State::RejectedTarget { .. } => {}
        }
        dialog.state = State::Terminated {
            expires: now.saturating_add(32_000),
        };
        Ok(Some(DialogReply::Accepted {
            bytes: crate::response(request, 200, "OK", "", "")?,
            events,
        }))
    }

    pub(crate) fn outgoing_ack(
        &mut self,
        request: &Request,
        route: SignalingRoute,
    ) -> Result<(), SipError> {
        let sequence = request.cseq_header()?.typed()?.seq;
        if let Some(dialog) = self.outgoing.get_mut(request.call_id_header()?.value()) {
            if dialog.matches(request, route)? {
                if let State::Established(media) = &mut dialog.state {
                    if media
                        .pending
                        .as_ref()
                        .is_some_and(|answer| sequence == answer.sequence)
                    {
                        media.pending = None;
                    }
                }
            }
        }
        Ok(())
    }
}
