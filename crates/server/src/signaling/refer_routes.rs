use super::{failure, key, transport, CallKey, ControlError, Output, Signal, Signaling, SipEvent};
use rusterisk_core::TransferToken;
use rusterisk_sip::{
    AttendedDialogRefer, DialogRefer, ReferFailure, ReferIdentity, ReferKey, ReferProgress,
};

const PER_ENGINE: usize = 64 + 256;

pub(super) struct ReferRoute {
    source: CallKey,
    route: transport::DeliveryRoute,
    identity: ReferIdentity,
    phase: Phase,
}

#[derive(Clone, Copy)]
enum Phase {
    Pending,
    Active(TransferToken),
    Settled,
}

impl Signaling {
    pub(super) fn prune_refer_routes(
        &mut self,
        listener: usize,
        now: u64,
    ) -> Result<(), ControlError> {
        let engine = &self
            .listeners
            .get(listener)
            .ok_or(ControlError::Invalid("Unknown REFER listener"))?
            .engine;
        let mut expired = Vec::new();
        for (reference, route) in &self.refer_routes {
            if reference.0 != listener || !matches!(route.phase, Phase::Settled) {
                continue;
            }
            if engine
                .refer_reply_deadline(&route.identity)
                .map_err(failure)?
                .is_none_or(|deadline| deadline <= now)
            {
                expired.push(*reference);
            }
        }
        for reference in expired {
            self.refer_routes.remove(&reference);
        }
        Ok(())
    }

    pub(super) fn settle_refer(&mut self, listener: usize, reference: ReferKey) {
        if let Some(route) = self.refer_routes.get_mut(&(listener, reference)) {
            route.phase = Phase::Settled;
        }
    }

    fn refer_events(&mut self, listener: usize, output: Output) -> Result<(), ControlError> {
        if !output.datagrams.is_empty() {
            return Err(ControlError::Invalid("Unexpected REFER cleanup packets"));
        }
        for event in output.events {
            match event {
                SipEvent::ReferSubscriptionEnded { refer } => self.settle_refer(listener, refer),
                SipEvent::AttendedTransferRequested { .. }
                | SipEvent::BlindTransferRequested { .. }
                | SipEvent::EndpointContact { .. }
                | SipEvent::ForkTeardownFailed { .. }
                | SipEvent::PeerDirection { .. }
                | SipEvent::DtmfReceived { .. }
                | SipEvent::Ringing { .. }
                | SipEvent::Answered { .. }
                | SipEvent::Ended { .. }
                | SipEvent::Incoming { .. }
                | SipEvent::IncomingEnded { .. } => {
                    return Err(ControlError::Invalid("Unexpected REFER cleanup event"));
                }
            }
        }
        Ok(())
    }

    pub(super) fn retire_refer(
        &mut self,
        listener: usize,
        reference: ReferKey,
        now: u64,
    ) -> Result<(), ControlError> {
        self.cancel_refer_wire(listener, reference)?;
        self.prune_refer_routes(listener, now)
    }

    pub(super) fn cancel_refer_wire(
        &mut self,
        listener: usize,
        reference: ReferKey,
    ) -> Result<(), ControlError> {
        self.settle_refer(listener, reference);
        let output = self
            .listeners
            .get_mut(listener)
            .ok_or(ControlError::Invalid("Unknown REFER listener"))?
            .engine
            .cancel_refer(reference);
        self.refer_events(listener, output)
    }

    pub(super) fn refer_delivery(
        &self,
        listener: usize,
        reference: ReferKey,
    ) -> Option<transport::DeliveryRoute> {
        self.refer_routes
            .get(&(listener, reference))
            .map(|route| route.route)
    }

    pub(super) async fn admit_refer(
        &mut self,
        listener: usize,
        id: String,
        generation: u64,
        refer: DialogRefer,
        now: u64,
    ) -> Result<(), ControlError> {
        let reference = refer.key();
        let source = key(id, generation)?;
        if !self.capture_refer(listener, &source, reference, now)? {
            return Ok(());
        }
        if self
            .controller
            .signal(Signal::BlindTransferRequested { key: source, refer })
            .await
            .is_err()
        {
            self.emit_refer(
                listener,
                reference,
                ReferProgress::Rejected(ReferFailure::Unavailable),
                now,
            )
            .await?;
        }
        Ok(())
    }

    pub(super) async fn admit_attended_refer(
        &mut self,
        listener: usize,
        id: String,
        generation: u64,
        refer: AttendedDialogRefer,
        now: u64,
    ) -> Result<(), ControlError> {
        let reference = refer.key();
        let source = key(id, generation)?;
        if !self.capture_refer(listener, &source, reference, now)? {
            return Ok(());
        }
        let consultation = match self.consultation(refer.target().replaces()) {
            Ok(consultation) => consultation,
            Err(failure) => {
                return self
                    .emit_refer(listener, reference, ReferProgress::Rejected(failure), now)
                    .await;
            }
        };
        if self
            .controller
            .signal(Signal::AttendedTransferRequested {
                key: source,
                refer: refer.into_source(),
                consultation,
            })
            .await
            .is_err()
        {
            self.emit_refer(
                listener,
                reference,
                ReferProgress::Rejected(ReferFailure::Unavailable),
                now,
            )
            .await?;
        }
        Ok(())
    }

    fn capture_refer(
        &mut self,
        listener: usize,
        source: &CallKey,
        reference: ReferKey,
        now: u64,
    ) -> Result<bool, ControlError> {
        self.prune_refer_routes(listener, now)?;
        if self
            .refer_routes
            .keys()
            .filter(|(index, _)| *index == listener)
            .count()
            >= PER_ENGINE
        {
            return Err(ControlError::Capacity);
        }
        if self.refer_routes.contains_key(&(listener, reference)) {
            return Err(ControlError::Conflict("REFER route already owned"));
        }
        let identity = self
            .listeners
            .get(listener)
            .ok_or(ControlError::Invalid("Unknown REFER listener"))?
            .engine
            .refer_identity(reference)
            .ok_or(ControlError::Invalid("REFER owner unavailable"))?;
        let route = self.network.capture(listener, identity.peer())?;
        self.refer_routes.insert(
            (listener, reference),
            ReferRoute {
                source: source.clone(),
                route,
                identity,
                phase: Phase::Pending,
            },
        );
        if !self.network.current(route) || self.controller.is_stopping() {
            self.retire_refer(listener, reference, now)?;
            return Ok(false);
        }
        Ok(true)
    }

    async fn emit_refer(
        &mut self,
        listener: usize,
        reference: ReferKey,
        progress: ReferProgress,
        now: u64,
    ) -> Result<(), ControlError> {
        let Some(route) = self.refer_delivery(listener, reference) else {
            return self.retire_refer(listener, reference, now);
        };
        if !self.network.current(route) {
            return self.retire_refer(listener, reference, now);
        }
        let output = self.listeners[listener]
            .engine
            .refer_progress(reference, progress, now)
            .map_err(failure)?;
        self.transmit(listener, output.datagrams).await?;
        self.refer_events(
            listener,
            Output {
                datagrams: Vec::new(),
                events: output.events,
            },
        )?;
        self.prune_refer_routes(listener, now)
    }

    pub(super) async fn reject_refer(
        &mut self,
        source: &CallKey,
        reference: ReferKey,
        failure: ReferFailure,
        now: u64,
    ) -> Result<(), ControlError> {
        let matched = self
            .refer_routes
            .iter()
            .find_map(|((listener, key), route)| {
                (*key == reference
                    && route.source == *source
                    && matches!(route.phase, Phase::Pending))
                .then_some(*listener)
            });
        if let Some(listener) = matched {
            self.settle_refer(listener, reference);
            self.emit_refer(listener, reference, ReferProgress::Rejected(failure), now)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn progress_refer(
        &mut self,
        source: &CallKey,
        reference: ReferKey,
        token: TransferToken,
        progress: ReferProgress,
        now: u64,
    ) -> Result<(), ControlError> {
        let matched = self
            .refer_routes
            .iter_mut()
            .find(|((_, key), route)| *key == reference && route.source == *source);
        let Some(((listener, _), route)) = matched else {
            return Ok(());
        };
        match route.phase {
            Phase::Pending
                if matches!(progress, ReferProgress::Trying | ReferProgress::Failed(_)) =>
            {
                route.phase = Phase::Active(token);
            }
            Phase::Active(current) if current == token => {}
            Phase::Pending | Phase::Active(_) | Phase::Settled => return Ok(()),
        }
        let listener = *listener;
        self.emit_refer(listener, reference, progress, now).await
    }

    pub(super) fn close_refer_routes(&mut self, now: u64) -> Result<(), ControlError> {
        let references: Vec<_> = self.refer_routes.keys().copied().collect();
        let result = super::cleanup::each(references, |(listener, reference)| {
            self.retire_refer(listener, reference, now)
        });
        self.refer_routes.clear();
        result
    }
}
