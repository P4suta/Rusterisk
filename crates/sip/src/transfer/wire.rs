use super::{request, ReferError};
use crate::SipError;
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method, Request};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Dialog {
    call_id: String,
    local: String,
    remote: String,
}

impl Dialog {
    pub fn remote(message: &impl HeadersExt) -> Result<Self, SipError> {
        let from = message.from_header()?.typed()?;
        let to = message.to_header()?.typed()?;
        Ok(Self {
            call_id: message.call_id_header()?.value().into(),
            local: to.tag().ok_or(SipError::InvalidPacket)?.value().into(),
            remote: from.tag().ok_or(SipError::InvalidPacket)?.value().into(),
        })
    }

    pub fn local(message: &impl HeadersExt) -> Result<Self, SipError> {
        let from = message.from_header()?.typed()?;
        let to = message.to_header()?.typed()?;
        Ok(Self {
            call_id: message.call_id_header()?.value().into(),
            local: from.tag().ok_or(SipError::InvalidPacket)?.value().into(),
            remote: to.tag().ok_or(SipError::InvalidPacket)?.value().into(),
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Notification {
    pub dialog: Dialog,
    pub refer: u32,
    pub sequence: u32,
}

impl Notification {
    pub fn from_request(request: &Request) -> Result<Option<Self>, SipError> {
        if request.method != Method::Notify {
            return Ok(None);
        }
        let Some(refer) = event_sequence(request)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            dialog: Dialog::local(request)?,
            refer,
            sequence: request.cseq_header()?.typed()?.seq,
        }))
    }

    pub fn same_subscription(&self, other: &Self) -> bool {
        self.dialog == other.dialog && self.refer == other.refer
    }
}

pub(crate) fn event_sequence(message: &impl HeadersExt) -> Result<Option<u32>, SipError> {
    match event(message)? {
        Event::Refer(sequence) => Ok(sequence),
        Event::Unsupported => Ok(None),
    }
}

pub(crate) enum Event {
    Refer(Option<u32>),
    Unsupported,
}

pub(crate) fn event(message: &impl HeadersExt) -> Result<Event, SipError> {
    let mut values = message
        .headers()
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("Event"));
    let Some(value) = values.next() else {
        return Ok(Event::Unsupported);
    };
    if values.next().is_some() {
        return Err(SipError::InvalidPacket);
    }
    let (package, parameters) = match value.value().split_once(';') {
        Some((_, "")) => return Err(SipError::InvalidPacket),
        Some(parts) => parts,
        None => (value.value(), ""),
    };
    if !package
        .trim_matches([' ', '\t'])
        .eq_ignore_ascii_case("refer")
    {
        return Ok(Event::Unsupported);
    }
    let mut found = false;
    let mut sequence = None;
    request::visit_parameters(parameters, |name, value| {
        if name.eq_ignore_ascii_case("id") {
            if found {
                return Err(ReferError::Invalid);
            }
            found = true;
            let value = value.ok_or(ReferError::Invalid)?;
            if let Ok(parsed) = value.parse::<u32>() {
                if parsed.to_string() == value {
                    sequence = Some(parsed);
                }
            }
        }
        Ok(())
    })
    .map_err(|error| match error {
        ReferError::Invalid | ReferError::Unsupported => SipError::InvalidPacket,
        ReferError::Capacity => SipError::Capacity,
    })?;
    Ok(Event::Refer(sequence))
}
