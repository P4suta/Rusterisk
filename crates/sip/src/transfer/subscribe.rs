use super::{
    dialog::MatchError,
    notify::{Notification, Status},
    owners::{self, State},
    progress::notification_sequence,
    wire,
};
use crate::{response, Engine, Output, RequestIdentity, SipError};
use rsipstack::sip::Request;
use std::net::SocketAddr;

fn expiration(request: &Request) -> Result<u32, SipError> {
    let mut values = request
        .headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("Expires"));
    let Some(value) = values.next() else {
        return Ok(600);
    };
    if values.next().is_some() {
        return Err(SipError::InvalidPacket);
    }
    let value = value.value().trim_matches([' ', '\t']);
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SipError::InvalidPacket);
    }
    Ok(value
        .parse::<u32>()
        .map_err(|_| SipError::InvalidPacket)?
        .min(600))
}

impl Engine {
    pub(crate) fn receive_refer_subscription(
        &mut self,
        identity: RequestIdentity,
        request: Request,
        peer: SocketAddr,
        now: u64,
    ) -> Result<Output, SipError> {
        let context = match self.incoming_refer_dialog(&request, peer) {
            Err(MatchError::Missing) => self.outgoing_refer_dialog(&request, peer),
            context => context,
        };
        let dialog = match context {
            Ok(dialog) => dialog,
            Err(MatchError::Missing) => {
                return Ok(Output::response(
                    peer,
                    self.render_response(&request, 481, "Call/Transaction Does Not Exist", "", "")?,
                ))
            }
            Err(MatchError::Rejected(code, phrase)) => {
                return Ok(Output::response(
                    peer,
                    response(&request, code, phrase, "", "")?,
                ))
            }
            Err(MatchError::Packet(error)) => return Err(error),
        };
        let sequence = match wire::event(&request) {
            Ok(wire::Event::Refer(Some(sequence))) => sequence,
            Ok(wire::Event::Unsupported) => {
                let bytes = response(&request, 489, "Bad Event", "", "")?;
                return self.cache_reply(identity, request, peer, now, bytes, Vec::new());
            }
            Ok(wire::Event::Refer(None)) => {
                let bytes = response(&request, 481, "Call/Transaction Does Not Exist", "", "")?;
                return self.cache_reply(identity, request, peer, now, bytes, Vec::new());
            }
            Err(_) => {
                let bytes = response(&request, 400, "Bad Request", "", "")?;
                return self.cache_reply(identity, request, peer, now, bytes, Vec::new());
            }
        };
        let key = self.refers.subscriptions.iter().find_map(|(key, owner)| {
            (owner.dialog.peer == peer
                && owner.dialog.wire == dialog.wire
                && owner.sequence == sequence
                && matches!(owner.state, State::Active { .. }))
            .then_some(*key)
        });
        let Some(key) = key else {
            let bytes = response(&request, 481, "Call/Transaction Does Not Exist", "", "")?;
            return self.cache_reply(identity, request, peer, now, bytes, Vec::new());
        };
        let duration = match expiration(&request) {
            Ok(duration) => duration,
            Err(_) => {
                let bytes = response(&request, 400, "Bad Request", "", "")?;
                return self.cache_reply(identity, request, peer, now, bytes, Vec::new());
            }
        };
        let target = match super::dialog::Target::from_subscribe(&request) {
            Ok(target) => target,
            Err(error) => {
                let (code, phrase) = match error {
                    crate::ContactError::Invalid => (400, "Bad Request"),
                    crate::ContactError::Unsupported => (501, "Not Implemented"),
                    crate::ContactError::Capacity => (503, "Service Unavailable"),
                };
                let bytes = response(&request, code, phrase, "", "")?;
                return self.cache_reply(identity, request, peer, now, bytes, Vec::new());
            }
        };
        let mut owner = self
            .refers
            .subscriptions
            .remove(&key)
            .ok_or(SipError::InvalidPacket)?;
        let State::Active { last, pending, .. } = owner.state else {
            return Err(SipError::InvalidPacket);
        };
        let current = pending.unwrap_or(last);
        let updated: Result<crate::Datagram, SipError> = (|| {
            let expires = now
                .checked_add(u64::from(duration) * 1000)
                .ok_or(SipError::Capacity)?;
            let state = if duration == 0 {
                super::notify::State::Terminated
            } else {
                super::notify::State::Active {
                    remaining: duration,
                }
            };
            let packet = self.refer_notification_to(
                &owner.dialog,
                Notification {
                    refer_sequence: owner.sequence,
                    status: Status::Provisional(current),
                    state,
                },
                Some(&target),
            )?;
            let notification_sequence = notification_sequence(&packet)?;
            owner.state = if duration == 0 {
                State::Terminal {
                    deadline: now
                        .checked_add(owners::TRANSACTION)
                        .ok_or(SipError::Capacity)?,
                    sequence: notification_sequence,
                }
            } else {
                State::Active {
                    last: current,
                    sent: now,
                    pending: None,
                }
            };
            owner.expires = expires;
            Ok(packet)
        })();
        self.refers.subscriptions.insert(key, owner);
        let packet = updated?;
        let bytes = response(
            &request,
            200,
            "OK",
            &format!("Expires: {duration}\r\nContact: {}\r\n", dialog.contact),
            "",
        )?;
        let mut output = self.cache_reply(identity, request, peer, now, bytes, Vec::new())?;
        output.datagrams.push(packet);
        Ok(output)
    }
}
