use crate::{Datagram, SipError};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Request, Response};
use std::collections::BTreeMap;
use std::net::SocketAddr;

enum Retry {
    Backoff,
    Provisional,
    Reliable,
}

struct Pending {
    request: Request,
    peer: SocketAddr,
    bytes: Vec<u8>,
    next: u64,
    interval: u64,
    expires: u64,
    retry: Retry,
    notification: Option<crate::transfer::wire::Notification>,
}

pub(super) enum Receipt {
    Unmatched,
    Consumed,
    Notification(NotificationReceipt),
}

pub(crate) enum NotificationOutcome {
    Response(u16),
    Timeout,
}

pub(crate) struct NotificationReceipt {
    pub notification: crate::transfer::wire::Notification,
    pub peer: SocketAddr,
    pub outcome: NotificationOutcome,
}

#[derive(Default)]
pub(crate) struct Transactions {
    requests: BTreeMap<String, Pending>,
}

fn key(message: &impl HeadersExt) -> Result<String, SipError> {
    let branch = message.transaction_id()?.ok_or(SipError::InvalidPacket)?;
    Ok(format!(
        "{}:{}:{branch}",
        message.call_id_header()?.value(),
        message.cseq_header()?.value()
    ))
}

impl Transactions {
    pub(super) fn disconnected(&mut self, peer: SocketAddr) {
        self.requests.retain(|_, pending| pending.peer != peer);
    }

    pub fn track(
        &mut self,
        request: Request,
        peer: SocketAddr,
        bytes: &[u8],
        now: u64,
        reliable: bool,
    ) -> Result<(), SipError> {
        self.requests
            .retain(|_, pending| pending.expires > now || pending.notification.is_some());
        let key = key(&request)?;
        if let Some(pending) = self.requests.get(&key) {
            return if pending.peer == peer && pending.request == request {
                Ok(())
            } else {
                Err(SipError::InvalidPacket)
            };
        }
        if self.requests.len() >= 128 {
            return Err(SipError::Capacity);
        }
        let notification = crate::transfer::wire::Notification::from_request(&request)?;
        self.requests.insert(
            key,
            Pending {
                request,
                peer,
                bytes: bytes.into(),
                next: now.saturating_add(500),
                interval: 500,
                expires: now.saturating_add(32_000),
                retry: if reliable {
                    Retry::Reliable
                } else {
                    Retry::Backoff
                },
                notification,
            },
        );
        Ok(())
    }

    pub fn receive(
        &mut self,
        response: &Response,
        peer: SocketAddr,
        now: u64,
    ) -> Result<Receipt, SipError> {
        let key = key(response)?;
        let Some(pending) = self.requests.get_mut(&key) else {
            return Ok(Receipt::Unmatched);
        };
        if pending.peer != peer
            || pending.expires <= now
            || pending.request.from_header()?.typed()? != response.from_header()?.typed()?
        {
            return Ok(Receipt::Unmatched);
        }
        if let Some(notification) = &pending.notification {
            if ["From", "To", "Call-ID", "CSeq"].into_iter().any(|name| {
                response
                    .headers
                    .iter()
                    .filter(|header| header.name().eq_ignore_ascii_case(name))
                    .count()
                    != 1
            }) {
                return Ok(Receipt::Unmatched);
            }
            match crate::transfer::wire::Dialog::local(response) {
                Ok(dialog) if dialog == notification.dialog => {}
                Ok(_) | Err(_) => return Ok(Receipt::Unmatched),
            }
        }
        let code = response.status_code.code();
        if (100..200).contains(&code) {
            if !matches!(pending.retry, Retry::Reliable) {
                pending.retry = Retry::Provisional;
            }
        } else if (200..700).contains(&code) {
            if let Some(Pending {
                notification: Some(notification),
                peer,
                ..
            }) = self.requests.remove(&key)
            {
                return Ok(Receipt::Notification(NotificationReceipt {
                    notification,
                    peer,
                    outcome: NotificationOutcome::Response(code),
                }));
            }
        } else {
            return Err(SipError::InvalidPacket);
        }
        Ok(Receipt::Consumed)
    }

    pub fn cancel_notification(
        &mut self,
        notification: &crate::transfer::wire::Notification,
        peer: SocketAddr,
    ) {
        self.requests.retain(|_, pending| {
            pending.peer != peer
                || !pending
                    .notification
                    .as_ref()
                    .is_some_and(|candidate| candidate.same_subscription(notification))
        });
    }

    pub(crate) fn cancel_refer(
        &mut self,
        dialog: &crate::transfer::wire::Dialog,
        sequence: u32,
        peer: SocketAddr,
    ) {
        self.requests.retain(|_, pending| {
            pending.peer != peer
                || !pending
                    .notification
                    .as_ref()
                    .is_some_and(|candidate| candidate.for_subscription(dialog, sequence))
        });
    }

    pub fn expire(&mut self, now: u64) -> Vec<NotificationReceipt> {
        let expired: Vec<_> = self
            .requests
            .iter()
            .filter_map(|(key, pending)| (pending.expires <= now).then_some(key.clone()))
            .collect();
        let mut notifications = Vec::new();
        for key in expired {
            if let Some(Pending {
                notification: Some(notification),
                peer,
                ..
            }) = self.requests.remove(&key)
            {
                notifications.push(NotificationReceipt {
                    notification,
                    peer,
                    outcome: NotificationOutcome::Timeout,
                });
            }
        }
        notifications
    }

    pub fn tick(&mut self, now: u64) -> Vec<Datagram> {
        let mut datagrams = Vec::new();
        for pending in self.requests.values_mut() {
            if pending.next <= now && !matches!(pending.retry, Retry::Reliable) {
                datagrams.push(Datagram {
                    peer: pending.peer,
                    bytes: pending.bytes.clone(),
                });
                pending.interval = match pending.retry {
                    Retry::Backoff => (pending.interval * 2).min(4000),
                    Retry::Provisional => 4000,
                    Retry::Reliable => pending.interval,
                };
                pending.next = now.saturating_add(pending.interval);
            }
        }
        datagrams
    }
}
