use crate::{SipError, MAX_MESSAGE};
use rsipstack::sip::{
    headers::untyped::ToTypedHeader,
    uri::{Branch, HostWithPort, Param, Tag},
    HeadersExt, Method, Request, Response, SipMessage, Transport,
};

pub(in crate::registration_client) struct RegistrationCorrelation {
    branch: Branch,
    sent_by: HostWithPort,
    transport: Transport,
    sequence: u32,
    call_id: String,
    from_tag: Option<Tag>,
}

struct Expected<'a> {
    branch: Option<&'a Branch>,
    sent_by: &'a HostWithPort,
    transport: Transport,
    sequence: u32,
    call_id: &'a str,
    from_tag: Option<&'a Tag>,
}

impl RegistrationCorrelation {
    pub(in crate::registration_client) fn capture(bytes: &[u8]) -> Result<Self, SipError> {
        if bytes.len() > MAX_MESSAGE {
            return Err(SipError::Capacity);
        }
        let request = match SipMessage::try_from(bytes)? {
            SipMessage::Request(request) => request,
            SipMessage::Response(_) => return Err(SipError::InvalidPacket),
        };
        let via = request.top_via_header()?.typed()?;
        let sequence = request.cseq_header()?.typed()?;
        if request.method != Method::Register || sequence.method != Method::Register {
            return Err(SipError::InvalidPacket);
        }
        let branch = via.branch().cloned().ok_or(SipError::InvalidPacket)?;
        Ok(Self {
            branch,
            sent_by: via.uri.host_with_port,
            transport: via.transport,
            sequence: sequence.seq,
            call_id: request.call_id_header()?.value().to_owned(),
            from_tag: request.from_header()?.typed()?.tag().cloned(),
        })
    }

    pub(in crate::registration_client) fn matches(
        &self,
        response: &Response,
    ) -> Result<bool, SipError> {
        Expected {
            branch: Some(&self.branch),
            sent_by: &self.sent_by,
            transport: self.transport,
            sequence: self.sequence,
            call_id: &self.call_id,
            from_tag: self.from_tag.as_ref(),
        }
        .matches(response)
    }

    pub(in crate::registration_client) fn matches_request(
        response: &Response,
        request: &Request,
    ) -> Result<bool, SipError> {
        let via = request.top_via_header()?.typed()?;
        let from = request.from_header()?.typed()?;
        Expected {
            branch: via.branch(),
            sent_by: via.sent_by(),
            transport: via.transport,
            sequence: request.cseq_header()?.typed()?.seq,
            call_id: request.call_id_header()?.value(),
            from_tag: from.tag(),
        }
        .matches(response)
    }
}

impl Expected<'_> {
    fn matches(self, response: &Response) -> Result<bool, SipError> {
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
        let sequence = response.cseq_header()?.typed()?;
        Ok(via.branch() == self.branch
            && via.sent_by() == self.sent_by
            && via.transport == self.transport
            && sequence.method == Method::Register
            && sequence.seq == self.sequence
            && response.call_id_header()?.value() == self.call_id
            && response.from_header()?.typed()?.tag() == self.from_tag)
    }
}
