use super::{
    dialog::{Dialog, MatchError, Usage},
    notify::{self, Notification, Provisional, Status},
    owners::{self, Pending, State, Subscription},
    request::{self, TransferTarget},
    AttendedDialogRefer, AttendedRefer, BlindRefer, DialogRefer, ReferError, ReferFailure,
    ReferKey, ReferProgress, ReferSubscription,
};
use crate::{
    response, Datagram, DatagramOrigin, Engine, Output, RequestIdentity, SignalingRoute, SipError,
    SipEvent,
};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Request};

fn empty() -> Output {
    Output {
        datagrams: Vec::new(),
        events: Vec::new(),
    }
}

impl Engine {
    pub(crate) fn receive_refer(
        &mut self,
        identity: RequestIdentity,
        request: Request,
        route: SignalingRoute,
        now: u64,
    ) -> Result<Output, SipError> {
        if self
            .refers
            .pending
            .values()
            .any(|pending| pending.identity == identity)
        {
            return Ok(empty());
        }
        let context = match self.incoming_refer_dialog(&request, route) {
            Err(MatchError::Missing) => self.outgoing_refer_dialog(&request, route),
            context => context,
        };
        let dialog = match context {
            Ok(dialog) => dialog,
            Err(MatchError::Missing) => {
                let (code, phrase) = if request.to_header()?.typed()?.tag().is_some() {
                    (481, "Call/Transaction Does Not Exist")
                } else {
                    (501, "Not Implemented")
                };
                return Ok(Output::response(
                    route,
                    self.render_response(&request, code, phrase, "", "")?,
                ));
            }
            Err(MatchError::Rejected(code, phrase)) => {
                return Ok(Output::response(
                    route,
                    response(&request, code, phrase, "", "")?,
                ))
            }
            Err(MatchError::Packet(error)) => return Err(error),
        };
        let rejection = match dialog.usage {
            Usage::Ended => Some((481, "Call/Transaction Does Not Exist")),
            Usage::Active if !dialog.authority.allow_transfer => Some((603, "Decline")),
            Usage::Active => None,
        };
        if let Some((code, phrase)) = rejection {
            let bytes = response(&request, code, phrase, "", "")?;
            return self.cache_reply(identity, request, route, now, bytes, Vec::new());
        }
        let target = match request::parse_transfer(&request) {
            Ok(target) => target,
            Err(error) => {
                let (code, phrase) = match error {
                    ReferError::Invalid => (400, "Bad Request"),
                    ReferError::Unsupported => (501, "Not Implemented"),
                    ReferError::Capacity => (503, "Service Unavailable"),
                };
                let bytes = response(&request, code, phrase, "", "")?;
                return self.cache_reply(identity, request, route, now, bytes, Vec::new());
            }
        };
        if matches!(&target.target, TransferTarget::Attended(_))
            && (dialog.security == crate::transport::UriSecurity::Sips
                || matches!(target.uri.scheme, Some(rsipstack::sip::uri::Scheme::Sips)))
        {
            let bytes = response(&request, 501, "Not Implemented", "", "")?;
            return self.cache_reply(identity, request, route, now, bytes, Vec::new());
        }
        let Some(deadline) = now
            .checked_add(owners::TRANSACTION)
            .filter(|_| self.refers.len() < owners::CAPACITY)
        else {
            let bytes = response(&request, 503, "Service Unavailable", "", "")?;
            return self.cache_reply(identity, request, route, now, bytes, Vec::new());
        };
        self.serial = self.serial.checked_add(1).ok_or(SipError::Capacity)?;
        let key = ReferKey(self.serial);
        let sequence = request.cseq_header()?.typed()?.seq;
        let subscription = target.subscription;
        let event = match target.target {
            TransferTarget::Blind(extension) => SipEvent::BlindTransferRequested {
                id: dialog.id.clone(),
                generation: dialog.generation,
                refer: DialogRefer {
                    key,
                    target: BlindRefer {
                        extension,
                        subscription,
                    },
                    authority: dialog.authority.clone(),
                },
            },
            TransferTarget::Attended(replaces) => SipEvent::AttendedTransferRequested {
                id: dialog.id.clone(),
                generation: dialog.generation,
                refer: AttendedDialogRefer {
                    key,
                    target: AttendedRefer {
                        uri: target.uri,
                        replaces,
                        subscription,
                    },
                    authority: dialog.authority.clone(),
                },
            },
        };
        let output = Output {
            datagrams: Vec::new(),
            events: [event].into_iter().collect(),
        };
        self.refers.pending.insert(
            key,
            Pending {
                dialog,
                request,
                identity,
                subscription,
                sequence,
                deadline,
            },
        );
        Ok(output)
    }

    pub fn refer_progress(
        &mut self,
        key: ReferKey,
        progress: ReferProgress,
        now: u64,
    ) -> Result<Output, SipError> {
        let output = self.refer_progress_inner(key, progress, now)?;
        self.track_output(&output, now)?;
        Ok(output)
    }

    fn refer_progress_inner(
        &mut self,
        key: ReferKey,
        progress: ReferProgress,
        now: u64,
    ) -> Result<Output, SipError> {
        let transaction = self
            .refers
            .pending
            .get(&key)
            .map(|owner| owner.identity.clone());
        let output = self.advance_refer(key, progress, now);
        if let Some(transaction) = transaction {
            if let Some(cached) = self.cache.get_mut(&transaction) {
                cached.origin = DatagramOrigin::Refer(key);
            }
        }
        let mut output = output?;
        for packet in &mut output.datagrams {
            packet.origin = DatagramOrigin::Refer(key);
        }
        Ok(output)
    }

    fn advance_refer(
        &mut self,
        key: ReferKey,
        progress: ReferProgress,
        now: u64,
    ) -> Result<Output, SipError> {
        let mut output = empty();
        if let Some(pending) = self.refers.pending.remove(&key) {
            if pending.subscription == ReferSubscription::Suppressed
                && progress == ReferProgress::Trying
            {
                self.refers.pending.insert(key, pending);
                return Ok(output);
            }
            let rejection = match progress {
                ReferProgress::Rejected(failure) => Some(failure),
                ReferProgress::Trying
                | ReferProgress::HandedOff
                | ReferProgress::Ringing
                | ReferProgress::Answered
                | ReferProgress::Failed(_) => None,
            };
            if let Some(failure) = rejection {
                return self.complete_pending(key, pending, failure.response(), "", now);
            }
            if pending.subscription == ReferSubscription::Suppressed {
                let status = match progress {
                    ReferProgress::Failed(failure) => failure.response(),
                    ReferProgress::Trying
                    | ReferProgress::HandedOff
                    | ReferProgress::Ringing
                    | ReferProgress::Answered
                    | ReferProgress::Rejected(_) => (200, "OK"),
                };
                return self.complete_pending(key, pending, status, "Refer-Sub: false\r\n", now);
            }
            let Some(expires) = now.checked_add(owners::LIFETIME) else {
                self.refers.pending.insert(key, pending);
                return Err(SipError::Capacity);
            };
            let notification = Notification {
                refer_sequence: pending.sequence,
                status: Status::Provisional(Provisional::Trying),
                state: notify::State::Active { remaining: 600 },
            };
            let packet = match self.refer_notification(key, &pending.dialog, notification) {
                Ok(packet) => packet,
                Err(error) => {
                    self.refers.pending.insert(key, pending);
                    return Err(error);
                }
            };
            let bytes = response(
                &pending.request,
                202,
                "Accepted",
                &format!("Contact: {}\r\n", pending.dialog.contact),
                "",
            )?;
            let owner = Subscription {
                dialog: pending.dialog,
                sequence: pending.sequence,
                expires,
                state: State::Active {
                    last: Provisional::Trying,
                    sent: now,
                    pending: None,
                },
            };
            output = self.cache_reply(
                pending.identity,
                pending.request,
                owner.dialog.route,
                now,
                bytes,
                Vec::new(),
            )?;
            output.datagrams.push(packet);
            self.refers.subscriptions.insert(key, owner);
        }
        let Some(mut owner) = self.refers.subscriptions.remove(&key) else {
            return Ok(output);
        };
        let updated = self.update_refer(key, &mut owner, progress, now);
        self.refers.subscriptions.insert(key, owner);
        output.datagrams.extend(updated?);
        Ok(output)
    }

    fn complete_pending(
        &mut self,
        key: ReferKey,
        pending: Pending,
        status: (u16, &str),
        extra: &str,
        now: u64,
    ) -> Result<Output, SipError> {
        let bytes = response(&pending.request, status.0, status.1, extra, "")?;
        self.cache_reply(
            pending.identity,
            pending.request,
            pending.dialog.route,
            now,
            bytes,
            [SipEvent::ReferSubscriptionEnded { refer: key }]
                .into_iter()
                .collect(),
        )
    }

    pub(super) fn refer_notification(
        &mut self,
        key: ReferKey,
        dialog: &Dialog,
        notification: Notification,
    ) -> Result<Datagram, SipError> {
        self.refer_notification_to(key, dialog, notification, None)
    }

    pub(super) fn refer_notification_to(
        &mut self,
        key: ReferKey,
        dialog: &Dialog,
        notification: Notification,
        target: Option<&super::dialog::Target>,
    ) -> Result<Datagram, SipError> {
        let mut packet = if let Some(packet) = self.incoming_notify(
            &dialog.id,
            dialog.generation,
            &dialog.contact,
            notification,
            target,
        )? {
            packet
        } else {
            self.outgoing_notify(
                &dialog.id,
                dialog.generation,
                &dialog.contact,
                notification,
                target,
            )?
            .ok_or(SipError::InvalidPacket)?
        };
        packet.origin = DatagramOrigin::Refer(key);
        Ok(packet)
    }

    fn update_refer(
        &mut self,
        key: ReferKey,
        owner: &mut Subscription,
        progress: ReferProgress,
        now: u64,
    ) -> Result<Vec<Datagram>, SipError> {
        let State::Active {
            last,
            sent,
            pending,
        } = &mut owner.state
        else {
            return Ok(Vec::new());
        };
        let final_status = match progress {
            ReferProgress::Trying => return Ok(Vec::new()),
            ReferProgress::HandedOff if owner.dialog.authority.blind_progress => {
                return Ok(Vec::new())
            }
            ReferProgress::HandedOff | ReferProgress::Answered => Some(Status::Succeeded),
            ReferProgress::Rejected(failure) | ReferProgress::Failed(failure) => {
                Some(Status::Failed(failure))
            }
            ReferProgress::Ringing => None,
        };
        if let Some(status) = final_status {
            let deadline = now
                .checked_add(owners::TRANSACTION)
                .ok_or(SipError::Capacity)?;
            let packet = self.refer_notification(
                key,
                &owner.dialog,
                Notification {
                    refer_sequence: owner.sequence,
                    status,
                    state: notify::State::Terminated,
                },
            )?;
            let sequence = notification_sequence(&packet)?;
            owner.state = State::Terminal { deadline, sequence };
            return Ok([packet].into_iter().collect());
        }
        if *last == Provisional::Ringing {
            return Ok(Vec::new());
        }
        if now.saturating_sub(*sent) < owners::RINGING_INTERVAL {
            *pending = Some(Provisional::Ringing);
            return Ok(Vec::new());
        }
        let remaining = owner
            .expires
            .saturating_sub(now)
            .div_ceil(1000)
            .try_into()
            .map_err(|_| SipError::Capacity)?;
        let packet = self.refer_notification(
            key,
            &owner.dialog,
            Notification {
                refer_sequence: owner.sequence,
                status: Status::Provisional(Provisional::Ringing),
                state: notify::State::Active { remaining },
            },
        )?;
        *last = Provisional::Ringing;
        *sent = now;
        *pending = None;
        Ok([packet].into_iter().collect())
    }

    pub(crate) fn refer_tick(&mut self, now: u64) -> Result<Output, SipError> {
        let mut output = empty();
        let pending: Vec<_> = self
            .refers
            .pending
            .iter()
            .filter_map(|(key, owner)| (owner.deadline <= now).then_some(*key))
            .collect();
        for key in pending {
            let expired = self.refer_progress_inner(
                key,
                ReferProgress::Rejected(ReferFailure::Unavailable),
                now,
            )?;
            output.datagrams.extend(expired.datagrams);
            output.events.extend(expired.events);
        }
        let keys: Vec<_> = self.refers.subscriptions.keys().copied().collect();
        for key in keys {
            let Some(mut owner) = self.refers.subscriptions.remove(&key) else {
                continue;
            };
            match owner.state {
                State::Terminal { deadline, .. } if deadline <= now => {
                    output
                        .events
                        .push(SipEvent::ReferSubscriptionEnded { refer: key });
                    continue;
                }
                State::Active { last, .. } if owner.expires <= now => {
                    let deadline = now
                        .checked_add(owners::TRANSACTION)
                        .ok_or(SipError::Capacity)?;
                    let packet = self.refer_notification(
                        key,
                        &owner.dialog,
                        Notification {
                            refer_sequence: owner.sequence,
                            status: Status::Provisional(last),
                            state: notify::State::Terminated,
                        },
                    )?;
                    let sequence = notification_sequence(&packet)?;
                    output.datagrams.push(packet);
                    owner.state = State::Terminal { deadline, sequence };
                }
                State::Active {
                    pending: Some(Provisional::Ringing),
                    ..
                } => output.datagrams.extend(self.update_refer(
                    key,
                    &mut owner,
                    ReferProgress::Ringing,
                    now,
                )?),
                State::Active {
                    pending: None | Some(Provisional::Trying),
                    ..
                }
                | State::Terminal { .. } => {}
            }
            self.refers.subscriptions.insert(key, owner);
        }
        Ok(output)
    }

    pub(crate) fn refer_receipt(
        &mut self,
        receipt: crate::transactions::NotificationReceipt,
    ) -> Output {
        use crate::transactions::NotificationOutcome;
        let mut output = empty();
        let matched = self.refers.subscriptions.iter().find_map(|(key, owner)| {
            if owner.dialog.route != receipt.route || owner.dialog.wire != receipt.notification.dialog || owner.sequence != receipt.notification.refer { return None; }
            let terminal = matches!(owner.state, State::Terminal { sequence, .. } if sequence == receipt.notification.sequence);
            let ended = match receipt.outcome {
                NotificationOutcome::Timeout => true,
                NotificationOutcome::Response(code) => terminal || matches!(code, 404 | 405 | 410 | 416 | 480..=485 | 489 | 501 | 604),
            };
            ended.then_some(*key)
        });
        if let Some(key) = matched {
            self.refers.subscriptions.remove(&key);
            self.transactions
                .cancel_notification(&receipt.notification, receipt.route);
            output
                .events
                .push(SipEvent::ReferSubscriptionEnded { refer: key });
        }
        output
    }
}

pub(super) fn notification_sequence(packet: &Datagram) -> Result<u32, SipError> {
    match rsipstack::sip::SipMessage::try_from(packet.bytes.as_slice())? {
        rsipstack::sip::SipMessage::Request(request)
            if request.method == rsipstack::sip::Method::Notify =>
        {
            Ok(request.cseq_header()?.typed()?.seq)
        }
        rsipstack::sip::SipMessage::Request(_) | rsipstack::sip::SipMessage::Response(_) => {
            Err(SipError::InvalidPacket)
        }
    }
}
