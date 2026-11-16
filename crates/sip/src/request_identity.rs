use crate::{Cached, SipError};
use rsipstack::sip::{
    headers::untyped::ToTypedHeader,
    uri::{Host, HostWithPort},
    HeadersExt, Method, Request,
};
use std::net::SocketAddr;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct RequestIdentity {
    peer: SocketAddr,
    transaction: Transaction,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Transaction {
    Cookie {
        branch: String,
        sent_by: HostWithPort,
        method: String,
    },
    Legacy {
        call_id: String,
        cseq: String,
        via: String,
    },
}

pub(super) fn prepare_request(request: &mut Request, wire: &[u8]) -> Result<(), SipError> {
    request.from_header()?.typed()?;
    request.to_header()?.typed()?;
    if request.call_id_header()?.value().is_empty() {
        return Err(SipError::InvalidPacket);
    }
    let forwards = request.max_forwards_header()?.value().trim();
    if forwards.is_empty()
        || !forwards.bytes().all(|byte| byte.is_ascii_digit())
        || forwards.parse::<u8>().is_err()
    {
        return Err(SipError::InvalidPacket);
    }
    let name = wire
        .split(|byte| *byte == b' ')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or(SipError::InvalidPacket)?;
    let name = std::str::from_utf8(name)?;
    if Method::try_from(name)? != request.method {
        return Err(SipError::InvalidPacket);
    }
    if request.method.to_string() != name {
        request.method = Method::Other(name.into());
    }
    let mut cseq = request
        .cseq_header()?
        .value()
        .split([' ', '\t'])
        .filter(|part| !part.is_empty());
    let sequence = cseq.next().ok_or(SipError::InvalidPacket)?;
    if !sequence.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SipError::InvalidPacket);
    }
    sequence
        .parse::<u32>()
        .map_err(|_| SipError::InvalidPacket)?;
    if cseq.next() != Some(name) || cseq.next().is_some() {
        return Err(SipError::InvalidPacket);
    }
    Ok(())
}

impl RequestIdentity {
    pub(super) fn for_invite(request: &Request, peer: SocketAddr) -> Result<Self, SipError> {
        let mut identity = Self::new(request, peer)?;
        match &mut identity.transaction {
            Transaction::Cookie { method, .. } => *method = "INVITE".into(),
            Transaction::Legacy { cseq, .. } => {
                *cseq = format!("{} INVITE", request.cseq_header()?.typed()?.seq)
            }
        }
        Ok(identity)
    }

    pub(super) fn new(request: &Request, peer: SocketAddr) -> Result<Self, SipError> {
        let via = request.top_via_header()?.typed()?;
        let transaction = if let Some(branch) = via
            .branch()
            .filter(|branch| branch.value().starts_with("z9hG4bK"))
        {
            let mut sent_by = via.sent_by().clone();
            if let Host::Domain(domain) = &mut sent_by.host {
                domain.0.make_ascii_lowercase();
            }
            Transaction::Cookie {
                branch: branch.to_string(),
                sent_by,
                method: request.method.to_string(),
            }
        } else {
            Transaction::Legacy {
                call_id: request.call_id_header()?.value().into(),
                cseq: request.cseq_header()?.value().into(),
                via: request.top_via_header()?.value().into(),
            }
        };
        Ok(Self { peer, transaction })
    }

    pub(super) fn accepts(&self, cached: &Cached, request: &Request) -> bool {
        match &self.transaction {
            Transaction::Cookie { .. } => true,
            Transaction::Legacy { .. } => cached.request == *request,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{prepare_request, RequestIdentity};
    use crate::SipError;
    use rsipstack::sip::SipMessage;

    fn identity(via: &str, method: &str, peer: u16) -> Result<RequestIdentity, SipError> {
        let wire = format!("{method} sip:100@localhost SIP/2.0\r\nVia: {via}\r\nFrom: <sip:caller@localhost>;tag=caller\r\nTo: <sip:100@localhost>\r\nCall-ID: call\r\nCSeq: 1 {method}\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n");
        let SipMessage::Request(mut request) = SipMessage::try_from(wire.as_str())? else {
            return Err(SipError::InvalidPacket);
        };
        prepare_request(&mut request, wire.as_bytes())?;
        RequestIdentity::new(&request, ([127, 0, 0, 1], peer).into())
    }

    #[test]
    fn cookie_identity_ignores_parameter_order_and_normalizes_domain_case() -> Result<(), SipError>
    {
        let original = identity(
            "SIP/2.0/UDP Phone.Example:5061;branch=z9hG4bK-A;rport",
            "INVITE",
            5061,
        )?;
        for via in [
            "SIP/2.0/UDP phone.example:5061;rport;branch=z9hG4bK-A",
            "SIP/2.0/UDP PHONE.EXAMPLE:5061;branch=z9hG4bK-A;received=127.0.0.1;rport=5061",
        ] {
            assert!(original == identity(via, "INVITE", 5061)?, "{via}");
        }
        Ok(())
    }

    #[test]
    fn branch_case_sent_by_method_and_peer_keep_transactions_distinct() -> Result<(), SipError> {
        let via = "SIP/2.0/UDP phone.example:5061;branch=z9hG4bK-A;rport";
        let original = identity(via, "INVITE", 5061)?;
        for changed in [
            via.replace("-A", "-a"),
            via.replace("phone.example", "another.example"),
            via.replace(":5061", ":5062"),
        ] {
            assert!(original != identity(&changed, "INVITE", 5061)?);
        }
        for method in ["CANCEL", "OPTIONS", "invite", "X-CUSTOM"] {
            assert!(original != identity(via, method, 5061)?, "{method}");
        }
        assert!(original != identity(via, "INVITE", 5062)?);
        Ok(())
    }
}
