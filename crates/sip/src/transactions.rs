use crate::{Datagram, DatagramOrigin, Disconnection, SignalingRoute, SipError};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Request, Response};
use std::collections::BTreeMap;

enum Retry {
    Backoff,
    Provisional,
    Reliable,
}

struct Pending {
    request: Request,
    route: SignalingRoute,
    bytes: Vec<u8>,
    next: u64,
    interval: u64,
    expires: u64,
    retry: Retry,
    notification: Option<crate::transfer::wire::Notification>,
    origin: DatagramOrigin,
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
    pub route: SignalingRoute,
    pub outcome: NotificationOutcome,
}

#[derive(Default)]
pub(crate) struct Transactions {
    requests: BTreeMap<(SignalingRoute, String), Pending>,
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
    pub(super) fn disconnected(&mut self, closed: Disconnection) {
        self.requests
            .retain(|_, pending| !closed.matches(pending.route));
    }

    pub fn track(
        &mut self,
        request: Request,
        route: SignalingRoute,
        bytes: &[u8],
        now: u64,
        reliable: bool,
        origin: DatagramOrigin,
    ) -> Result<(), SipError> {
        self.requests
            .retain(|_, pending| pending.expires > now || pending.notification.is_some());
        let key = (route, key(&request)?);
        if let Some(pending) = self.requests.get(&key) {
            return if pending.route == route
                && pending.request == request
                && pending.origin == origin
            {
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
                route,
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
                origin,
            },
        );
        Ok(())
    }

    pub fn receive(
        &mut self,
        response: &Response,
        route: SignalingRoute,
        now: u64,
    ) -> Result<Receipt, SipError> {
        let key = (route, key(response)?);
        let Some(pending) = self.requests.get_mut(&key) else {
            return Ok(Receipt::Unmatched);
        };
        if pending.route != route
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
                route,
                ..
            }) = self.requests.remove(&key)
            {
                return Ok(Receipt::Notification(NotificationReceipt {
                    notification,
                    route,
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
        route: SignalingRoute,
    ) {
        self.requests.retain(|_, pending| {
            pending.route != route
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
        route: SignalingRoute,
    ) {
        self.requests.retain(|_, pending| {
            pending.route != route
                || !pending
                    .notification
                    .as_ref()
                    .is_some_and(|candidate| candidate.for_subscription(dialog, sequence))
        });
    }

    pub(super) fn len(&self) -> usize {
        self.requests.len()
    }

    pub(super) fn wake(&self) -> rusterisk_core::Wake {
        use rusterisk_core::Wake;
        self.requests.values().fold(Wake::Idle, |wake, pending| {
            let wake = wake.earliest(Wake::At(pending.expires));
            if matches!(pending.retry, Retry::Reliable) {
                wake
            } else {
                wake.earliest(Wake::At(pending.next))
            }
        })
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
                route,
                ..
            }) = self.requests.remove(&key)
            {
                notifications.push(NotificationReceipt {
                    notification,
                    route,
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
                    peer: pending.route.peer,
                    bytes: pending.bytes.clone(),
                    origin: pending.origin,
                    flow: pending.route.flow,
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
