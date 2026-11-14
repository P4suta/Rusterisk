use crate::{response, Engine, Output, SipError};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Request};

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

impl Engine {
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
        for cached in self
            .cache
            .values_mut()
            .filter(|cached| cached.peer == invite.peer && cached.request == invite.request)
        {
            cached.bytes = bytes.clone();
            cached.expires = now.saturating_add(32_000);
        }
        Ok(Output::response(invite.peer, bytes))
    }
}
