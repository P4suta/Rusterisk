use super::{RegistrationIdentity, RegistrationTransaction, RegistrationTransactionEvent, State};
use crate::{Datagram, SipError, StreamFramingError, MAX_MESSAGE};
use rsipstack::sip::{
    headers::untyped::ToTypedHeader, uri::Param, HeadersExt, Method, Request, Response, SipMessage,
    Version,
};
use rusterisk_config::Protocol;
use std::net::SocketAddr;

pub struct RegistrationResponse {
    response: Response,
    request: Datagram,
    identity: RegistrationIdentity,
    protocol: Protocol,
}

impl RegistrationResponse {
    pub fn status(&self) -> u16 {
        self.response.status_code.code()
    }

    pub const fn identity(&self) -> RegistrationIdentity {
        self.identity
    }

    pub const fn protocol(&self) -> Protocol {
        self.protocol
    }

    pub const fn peer(&self) -> SocketAddr {
        self.request.peer
    }

    pub fn header_values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.response
            .headers
            .iter()
            .filter(move |header| header.name().eq_ignore_ascii_case(name))
            .map(|header| header.value())
    }

    pub fn request_uri(&self) -> Result<&str, SipError> {
        std::str::from_utf8(&self.request.bytes)?
            .split_once("\r\n")
            .and_then(|(line, _)| line.strip_prefix("REGISTER "))
            .and_then(|line| line.strip_suffix(" SIP/2.0"))
            .ok_or(SipError::InvalidPacket)
    }
}

impl RegistrationTransaction {
    pub fn receive(
        &mut self,
        bytes: &[u8],
        peer: SocketAddr,
        now: u64,
    ) -> Result<Option<RegistrationTransactionEvent>, SipError> {
        let retry = match self.state {
            State::Trying(retry) | State::Proceeding(retry) => retry,
            State::Ready { .. } | State::Completed { .. } | State::Terminated => return Ok(None),
        };
        if peer != self.packet.peer || now >= self.expires {
            return Ok(None);
        }
        if now < self.observed {
            return Err(SipError::InvalidPacket);
        }
        let response = match parse(bytes, self.protocol)? {
            SipMessage::Response(response) => response,
            SipMessage::Request(_) => return Ok(None),
        };
        let request = match SipMessage::try_from(self.packet.bytes.as_slice())? {
            SipMessage::Request(request) => request,
            SipMessage::Response(_) => return Err(SipError::InvalidPacket),
        };
        if !matches_request(&response, &request)? {
            return Ok(None);
        }
        let status = response.status_code.code();
        if !(100..=699).contains(&status) || response.version != Version::V2 {
            return Err(SipError::InvalidPacket);
        }
        self.observed = now;
        if status < 200 {
            self.state = State::Proceeding(retry);
            return Ok(Some(RegistrationTransactionEvent::Provisional(status)));
        }
        self.state = match self.protocol {
            Protocol::Udp => State::Completed {
                until: now.saturating_add(5000),
            },
            Protocol::Tcp | Protocol::Tls => State::Terminated,
            Protocol::Ws | Protocol::Wss => return Err(SipError::Unsupported),
        };
        Ok(Some(RegistrationTransactionEvent::Response(
            RegistrationResponse {
                response,
                request: Datagram {
                    peer,
                    bytes: std::mem::take(&mut self.packet.bytes),
                },
                identity: self.identity,
                protocol: self.protocol,
            },
        )))
    }
}

fn parse(bytes: &[u8], protocol: Protocol) -> Result<SipMessage, SipError> {
    if bytes.len() > MAX_MESSAGE {
        return Err(SipError::Capacity);
    }
    crate::validate_websocket_message(bytes).map_err(framing_error)?;
    if protocol != Protocol::Udp {
        let end = bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or(SipError::InvalidPacket)?;
        crate::stream_framing::body_length(&bytes[..end]).map_err(framing_error)?;
    }
    Ok(SipMessage::try_from(bytes)?)
}

fn framing_error(error: StreamFramingError) -> SipError {
    if matches!(error, StreamFramingError::Capacity) {
        SipError::Capacity
    } else {
        SipError::InvalidPacket
    }
}

fn matches_request(response: &Response, request: &Request) -> Result<bool, SipError> {
    for name in ["Call-ID", "CSeq", "From", "To"] {
        if response
            .headers
            .iter()
            .filter(|header| header.name().eq_ignore_ascii_case(name))
            .count()
            != 1
        {
            return Err(SipError::InvalidPacket);
        }
    }
    let via = response.top_via_header()?.typed()?;
    if via
        .params
        .iter()
        .filter(|parameter| matches!(parameter, Param::Branch(_)))
        .count()
        != 1
    {
        return Err(SipError::InvalidPacket);
    }
    let expected = request.top_via_header()?.typed()?;
    let sequence = response.cseq_header()?.typed()?;
    Ok(via.branch() == expected.branch()
        && via.sent_by() == expected.sent_by()
        && via.transport == expected.transport
        && sequence.method == Method::Register
        && sequence.seq == request.cseq_header()?.typed()?.seq
        && response.call_id_header()?.value() == request.call_id_header()?.value()
        && response.from_header()?.typed()?.tag() == request.from_header()?.typed()?.tag())
}
