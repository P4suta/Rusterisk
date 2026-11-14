use crate::{Datagram, SipError};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Request, Response};
use std::collections::BTreeMap;
use std::net::SocketAddr;

enum Retry {
    Backoff,
    Provisional,
}

struct Pending {
    request: Request,
    peer: SocketAddr,
    bytes: Vec<u8>,
    next: u64,
    interval: u64,
    expires: u64,
    retry: Retry,
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
    pub fn track(
        &mut self,
        request: Request,
        peer: SocketAddr,
        bytes: &[u8],
        now: u64,
    ) -> Result<(), SipError> {
        self.requests.retain(|_, pending| pending.expires > now);
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
        self.requests.insert(
            key,
            Pending {
                request,
                peer,
                bytes: bytes.into(),
                next: now.saturating_add(500),
                interval: 500,
                expires: now.saturating_add(32_000),
                retry: Retry::Backoff,
            },
        );
        Ok(())
    }

    pub fn receive(
        &mut self,
        response: &Response,
        peer: SocketAddr,
        now: u64,
    ) -> Result<bool, SipError> {
        let key = key(response)?;
        let Some(pending) = self.requests.get_mut(&key) else {
            return Ok(false);
        };
        if pending.peer != peer
            || pending.expires <= now
            || pending.request.from_header()?.typed()? != response.from_header()?.typed()?
        {
            return Ok(false);
        }
        let code = response.status_code.code();
        if (100..200).contains(&code) {
            pending.retry = Retry::Provisional;
        } else if (200..700).contains(&code) {
            self.requests.remove(&key);
        } else {
            return Err(SipError::InvalidPacket);
        }
        Ok(true)
    }

    pub fn tick(&mut self, now: u64) -> Vec<Datagram> {
        self.requests.retain(|_, pending| pending.expires > now);
        let mut datagrams = Vec::new();
        for pending in self.requests.values_mut() {
            if pending.next <= now {
                datagrams.push(Datagram {
                    peer: pending.peer,
                    bytes: pending.bytes.clone(),
                });
                pending.interval = match pending.retry {
                    Retry::Backoff => (pending.interval * 2).min(4000),
                    Retry::Provisional => 4000,
                };
                pending.next = now.saturating_add(pending.interval);
            }
        }
        datagrams
    }
}
