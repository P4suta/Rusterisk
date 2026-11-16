use super::ReferFailure;
use crate::SipError;
use rsipstack::sip::{
    headers::untyped::{Contact, ContentLength, ContentType, Event, SubscriptionState},
    Header, Request,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Provisional {
    Trying,
    Ringing,
}

#[derive(Clone, Copy)]
pub(crate) enum Status {
    Provisional(Provisional),
    Succeeded,
    Failed(ReferFailure),
}

impl Status {
    pub const fn response(self) -> (u16, &'static str) {
        match self {
            Self::Provisional(Provisional::Trying) => (100, "Trying"),
            Self::Provisional(Provisional::Ringing) => (180, "Ringing"),
            Self::Succeeded => (200, "OK"),
            Self::Failed(failure) => failure.response(),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum State {
    Active { remaining: u32 },
    Terminated,
}

#[derive(Clone, Copy)]
pub(crate) struct Notification {
    pub refer_sequence: u32,
    pub status: Status,
    pub state: State,
}

pub(crate) fn encode(
    mut request: Request,
    contact: &str,
    notification: Notification,
) -> Result<Vec<u8>, SipError> {
    let (code, phrase) = notification.status.response();
    let body = format!("SIP/2.0 {code} {phrase}\r\n");
    let state = match notification.state {
        State::Active { remaining } => format!("active;expires={remaining}"),
        State::Terminated => "terminated;reason=noresource".into(),
    };
    request
        .headers
        .unique_push(Header::Event(Event::new(format!(
            "refer;id={}",
            notification.refer_sequence
        ))));
    request
        .headers
        .unique_push(Header::SubscriptionState(SubscriptionState::new(state)));
    request
        .headers
        .unique_push(Header::ContentType(ContentType::new(
            "message/sipfrag;version=2.0",
        )));
    request
        .headers
        .unique_push(Header::ContentLength(ContentLength::new(
            body.len().to_string(),
        )));
    request
        .headers
        .unique_push(Header::Contact(Contact::new(contact)));
    request.body = body.into_bytes();
    let bytes = request.to_string().into_bytes();
    if bytes.len() > crate::MAX_MESSAGE {
        return Err(SipError::Capacity);
    }
    Ok(bytes)
}
