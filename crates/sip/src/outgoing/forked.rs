use super::{related_to, request, Outgoing};
use crate::{
    dialog_target::DialogTarget, transactions::Transactions, Datagram, DatagramOrigin, Output,
    SignalingRoute, SipError, SipEvent, MAX_MESSAGE,
};
use rsipstack::sip::{headers::untyped::ToTypedHeader, Header, HeadersExt, Method, Response};
use sha2::{Digest, Sha256};

const CAPACITY: usize = 128;
const BUFFER_CAPACITY: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub enum ForkTeardownFailure {
    TransactionCapacity,
    RetentionCapacity,
    RequestConstruction(SipError),
    TransactionRejected(SipError),
}

enum Teardown {
    Pending,
    Sent,
}

struct RetainedAck {
    fingerprint: [u8; 32],
    until: u64,
    ack: Vec<u8>,
    teardown: Teardown,
    route: SignalingRoute,
}

pub(crate) struct Forks {
    entries: [Option<RetainedAck>; CAPACITY],
    retained_bytes: usize,
}

impl Default for Forks {
    fn default() -> Self {
        Self {
            entries: std::array::from_fn(|_| None),
            retained_bytes: 0,
        }
    }
}

enum Retention<'a> {
    Stored(&'a mut RetainedAck),
    Refused(Vec<u8>),
}

impl Forks {
    pub(super) fn expire(&mut self, now: u64) -> Result<(), SipError> {
        for entry in &mut self.entries {
            if let Some(retained) = entry.as_ref().filter(|retained| retained.until <= now) {
                let remaining = self
                    .retained_bytes
                    .checked_sub(retained.ack.capacity())
                    .ok_or(SipError::InvalidPacket)?;
                *entry = None;
                self.retained_bytes = remaining;
            }
        }
        Ok(())
    }

    fn retained(&mut self, fingerprint: [u8; 32]) -> Option<&mut RetainedAck> {
        self.entries
            .iter_mut()
            .flatten()
            .find(|retained| retained.fingerprint == fingerprint)
    }

    fn retain(
        &mut self,
        fingerprint: [u8; 32],
        until: u64,
        ack: Vec<u8>,
        route: SignalingRoute,
    ) -> Retention<'_> {
        let Some(retained_bytes) = self
            .retained_bytes
            .checked_add(ack.capacity())
            .filter(|retained| *retained <= BUFFER_CAPACITY)
        else {
            return Retention::Refused(ack);
        };
        match self.entries.iter_mut().find(|entry| entry.is_none()) {
            Some(slot) => {
                self.retained_bytes = retained_bytes;
                Retention::Stored(slot.insert(RetainedAck {
                    fingerprint,
                    until,
                    ack,
                    teardown: Teardown::Pending,
                    route,
                }))
            }
            None => Retention::Refused(ack),
        }
    }
}

fn fingerprint(dialog: &Outgoing, response: &Response) -> Result<[u8; 32], SipError> {
    let via = dialog.invite.top_via_header()?.typed()?;
    let branch = via.branch().ok_or(SipError::InvalidPacket)?;
    let remote = response.to_header()?.typed()?;
    let mut hash = Sha256::new();
    hash.update(b"rusterisk-unselected-dialog\0");
    for value in [
        dialog.invite.call_id_header()?.value(),
        dialog.invite.from_header()?.value(),
        branch.value(),
    ] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    match remote.tag() {
        Some(tag) => {
            hash.update([1]);
            hash.update(tag.value().as_bytes());
        }
        None => hash.update([0]),
    }
    Ok(hash.finalize().into())
}

fn packet(
    dialog: &Outgoing,
    method: Method,
    to: &str,
    sequence: u32,
    target: &DialogTarget,
    fingerprint: [u8; 32],
) -> Result<Vec<u8>, SipError> {
    let bytes = related_to(dialog, method, to, sequence, true, target)?;
    let mut packet = request(std::str::from_utf8(&bytes)?)?;
    let via = packet.top_via_header()?.typed()?;
    let branch = via.branch().ok_or(SipError::InvalidPacket)?;
    let branch = format!("{branch}-fork-{}", crate::hexadecimal(fingerprint)?);
    let header = packet
        .headers
        .iter_mut()
        .find(|header| header.name().eq_ignore_ascii_case("Via"))
        .ok_or(SipError::InvalidPacket)?;
    *header = Header::Via(via.with_branch(branch.into()).to_string().into());
    let bytes = packet.to_string().into_bytes();
    if bytes.len() > MAX_MESSAGE {
        return Err(SipError::Capacity);
    }
    Ok(bytes)
}

fn failed(dialog: &Outgoing, output: &mut Output, failure: ForkTeardownFailure) {
    output.events.push(SipEvent::ForkTeardownFailed {
        id: dialog.id.clone(),
        generation: dialog.generation,
        failure,
    });
}

pub(super) fn receive(
    dialog: &Outgoing,
    forks: &mut Forks,
    transactions: &mut Transactions,
    response: &Response,
    until: u64,
    now: u64,
) -> Result<Output, SipError> {
    let fingerprint = fingerprint(dialog, response)?;
    forks.expire(now)?;
    if let Some(retained) = forks.retained(fingerprint) {
        if retained.route != dialog.route {
            return Err(SipError::InvalidPacket);
        }
        return Ok(acknowledge(dialog, retained, transactions, now));
    }
    let target = DialogTarget::from_response(dialog.protocol, dialog.uri_security, response)?
        .ok_or(SipError::InvalidPacket)?;
    let remote = response.to_header()?.value();
    let sequence = dialog.invite.cseq_header()?.typed()?.seq;
    let ack = packet(dialog, Method::Ack, remote, sequence, &target, fingerprint)?;
    match forks.retain(fingerprint, until, ack, dialog.route) {
        Retention::Stored(retained) => Ok(acknowledge(dialog, retained, transactions, now)),
        Retention::Refused(ack) => {
            let mut output = Output::response(dialog.route, ack);
            failed(dialog, &mut output, ForkTeardownFailure::RetentionCapacity);
            Ok(output)
        }
    }
}

fn acknowledge(
    dialog: &Outgoing,
    retained: &mut RetainedAck,
    transactions: &mut Transactions,
    now: u64,
) -> Output {
    let mut output = Output::response(retained.route, retained.ack.clone());
    match retained.teardown {
        Teardown::Sent => return output,
        Teardown::Pending => {}
    }
    let bye = (|| {
        let ack = request(std::str::from_utf8(&retained.ack)?)?;
        let sequence = ack
            .cseq_header()?
            .typed()?
            .seq
            .checked_add(1)
            .filter(|sequence| *sequence < 1 << 31)
            .ok_or(SipError::Capacity)?;
        let target = DialogTarget::from_uri(dialog.protocol, dialog.uri_security, &ack.uri)?;
        packet(
            dialog,
            Method::Bye,
            ack.to_header()?.value(),
            sequence,
            &target,
            retained.fingerprint,
        )
    })();
    let bye = match bye {
        Ok(bye) => bye,
        Err(error) => {
            failed(
                dialog,
                &mut output,
                ForkTeardownFailure::RequestConstruction(error),
            );
            return output;
        }
    };
    let tracked = (|| {
        transactions.track(
            request(std::str::from_utf8(&bye)?)?,
            retained.route,
            &bye,
            now,
            dialog.reliable,
            DatagramOrigin::Direct,
        )
    })();
    match tracked {
        Ok(()) => {
            retained.teardown = Teardown::Sent;
            output
                .datagrams
                .push(Datagram::on_route(retained.route, bye));
        }
        Err(SipError::Capacity) => failed(
            dialog,
            &mut output,
            ForkTeardownFailure::TransactionCapacity,
        ),
        Err(error) => failed(
            dialog,
            &mut output,
            ForkTeardownFailure::TransactionRejected(error),
        ),
    }
    output
}

#[cfg(test)]
#[path = "forked/retention_tests.rs"]
mod tests;
