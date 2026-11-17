use super::{dialog, wire, ReferKey};
use crate::{
    Datagram, DatagramOrigin, Engine, Output, SignalingRoute, SipError, SipEvent, StreamFlowId,
    MAX_MESSAGE,
};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method, SipMessage};
use std::net::SocketAddr;

pub struct ReferIdentity {
    key: ReferKey,
    dialog: wire::Dialog,
    route: SignalingRoute,
    sequence: u32,
}

fn headers(message: &impl HeadersExt) -> Result<(), SipError> {
    if message.headers().len() > 64 {
        return Err(SipError::Capacity);
    }
    for name in ["From", "To", "Call-ID", "CSeq"] {
        if message
            .headers()
            .iter()
            .filter(|header| header.name().eq_ignore_ascii_case(name))
            .count()
            != 1
        {
            return Err(SipError::InvalidPacket);
        }
    }
    Ok(())
}

impl ReferIdentity {
    fn capture(key: ReferKey, dialog: &dialog::Dialog, sequence: u32) -> Self {
        Self {
            key,
            dialog: dialog.wire.clone(),
            route: dialog.route,
            sequence,
        }
    }

    pub const fn peer(&self) -> SocketAddr {
        self.route.peer
    }

    pub const fn flow(&self) -> Option<StreamFlowId> {
        self.route.flow
    }

    pub fn matches(&self, packet: &Datagram) -> Result<bool, SipError> {
        if packet.bytes.len() > MAX_MESSAGE {
            return Err(SipError::Capacity);
        }
        if packet.route() != self.route {
            return Ok(false);
        }
        match SipMessage::try_from(packet.bytes.as_slice())? {
            SipMessage::Request(request) => {
                headers(&request)?;
                let Some(notification) = wire::Notification::from_request(&request)? else {
                    return Ok(false);
                };
                Ok(request.cseq_header()?.typed()?.method == Method::Notify
                    && notification.for_subscription(&self.dialog, self.sequence)
                    && packet.origin == DatagramOrigin::Refer(self.key))
            }
            SipMessage::Response(response) => {
                headers(&response)?;
                let cseq = response.cseq_header()?.typed()?;
                Ok(cseq.method == Method::Refer
                    && cseq.seq == self.sequence
                    && wire::Dialog::remote(&response)? == self.dialog
                    && packet.origin == DatagramOrigin::Refer(self.key))
            }
        }
    }
}

impl Engine {
    pub fn refer_identity(&self, key: ReferKey) -> Option<ReferIdentity> {
        self.refers
            .pending
            .get(&key)
            .map(|owner| ReferIdentity::capture(key, &owner.dialog, owner.sequence))
            .or_else(|| {
                self.refers
                    .subscriptions
                    .get(&key)
                    .map(|owner| ReferIdentity::capture(key, &owner.dialog, owner.sequence))
            })
    }

    pub fn cancel_refer(&mut self, key: ReferKey) -> Output {
        let owner = self
            .refers
            .pending
            .remove(&key)
            .map(|owner| (owner.dialog, owner.sequence))
            .or_else(|| {
                self.refers
                    .subscriptions
                    .remove(&key)
                    .map(|owner| (owner.dialog, owner.sequence))
            });
        let events = if let Some((dialog, sequence)) = owner {
            self.transactions
                .cancel_refer(&dialog.wire, sequence, dialog.route);
            [SipEvent::ReferSubscriptionEnded { refer: key }]
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        Output {
            datagrams: Vec::new(),
            events,
        }
    }
}

impl Engine {
    pub fn refer_reply_deadline(&self, identity: &ReferIdentity) -> Result<Option<u64>, SipError> {
        let mut deadline = None;
        for cached in self.cache.values() {
            if cached.origin != DatagramOrigin::Refer(identity.key)
                || cached.route != identity.route
                || cached.request.method != Method::Refer
            {
                continue;
            }
            let sequence = cached.request.cseq_header()?.typed()?;
            if sequence.method != Method::Refer
                || sequence.seq != identity.sequence
                || wire::Dialog::remote(&cached.request)? != identity.dialog
            {
                continue;
            }
            let expires = cached.expires.ok_or(SipError::InvalidPacket)?;
            deadline = Some(deadline.map_or(expires, |previous: u64| previous.max(expires)));
        }
        Ok(deadline)
    }
}
