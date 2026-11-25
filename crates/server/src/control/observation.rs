use super::{subscriptions::EndedSource, Control, ControlError, Outcome};
use crate::runner::{WatchFilter, WatchTarget};
use rusterisk_api::{
    observation::{ObservedEndpoint, ObservedLeg, WatchEntry, WatchRecord},
    Channel, ChannelId, Endpoint, EventKind,
};

fn endpoint(value: &Endpoint) -> ObservedEndpoint {
    ObservedEndpoint {
        technology: value.technology.clone(),
        resource: value.resource.clone(),
        state: value.state,
    }
}

impl Control {
    fn observed_leg(&self, channel: &Channel, ended: &[EndedSource]) -> Option<ObservedLeg> {
        let id = &channel.id;
        let (generation, endpoint) = ended
            .iter()
            .find_map(|source| match source {
                EndedSource::Channel { key, endpoint } if key.id == channel.id => {
                    Some((key.generation, endpoint))
                }
                EndedSource::Channel { .. } | EndedSource::Bridge { .. } => None,
            })
            .or_else(|| {
                self.channels
                    .get(id)
                    .map(|entry| (entry.call.generation(), &entry.endpoint))
            })?;
        let bridge_ids = self
            .bridges
            .iter()
            .filter(|(_, entry)| entry.snapshot.channels.contains(&channel.id))
            .map(|(id, _)| id.clone())
            .collect();
        Some(ObservedLeg {
            id: channel.id.clone(),
            generation,
            state: channel.state,
            endpoint: endpoint.clone(),
            bridge_ids,
        })
    }

    pub(crate) fn watch_snapshot(
        &self,
        target: WatchTarget,
    ) -> Result<(WatchFilter, WatchEntry), ControlError> {
        match target {
            WatchTarget::Channel(id) => {
                let key = self.key(&id)?;
                let leg = self
                    .observed_leg(self.channel(&id)?, &[])
                    .ok_or(ControlError::NotFound("Channel not found"))?;
                Ok((
                    WatchFilter::Channel(key),
                    WatchEntry::Snapshot {
                        channels: std::iter::once(leg).collect(),
                        endpoint: None,
                    },
                ))
            }
            WatchTarget::Endpoint {
                technology,
                resource,
            } => {
                let selected = self.endpoint(&technology, &resource)?;
                let mut channels = Vec::with_capacity(selected.channel_ids.len());
                for id in &selected.channel_ids {
                    if let Some(leg) = self.observed_leg(self.channel(id)?, &[]) {
                        channels.push(leg);
                    }
                }
                Ok((
                    WatchFilter::Endpoint(resource),
                    WatchEntry::Snapshot {
                        channels,
                        endpoint: Some(endpoint(&selected)),
                    },
                ))
            }
        }
    }

    pub(crate) fn watch_records(&self, outcome: &Outcome, mut emit: impl FnMut(WatchRecord)) {
        for notice in &outcome.notices {
            let kind = &notice.kind;
            let mut channel_event = |channel: &Channel, bridge_id: Option<&ChannelId>| {
                if let Some(leg) = self.observed_leg(channel, &outcome.ended_sources) {
                    emit(WatchRecord {
                        timestamp: notice.timestamp.clone(),
                        entry: WatchEntry::Event {
                            event: kind.event_type().into(),
                            channel: Some(leg),
                            endpoint: None,
                            bridge_id: bridge_id.cloned(),
                        },
                    });
                }
            };
            match kind {
                EventKind::ChannelCreated { channel }
                | EventKind::ChannelDestroyed { channel, .. }
                | EventKind::ChannelStateChange { channel }
                | EventKind::ChannelHangupRequest { channel, .. }
                | EventKind::ChannelHold { channel, .. }
                | EventKind::ChannelUnhold { channel }
                | EventKind::ChannelDialplan { channel, .. }
                | EventKind::ChannelCallerId { channel, .. }
                | EventKind::ChannelVarset { channel, .. }
                | EventKind::ChannelDtmfReceived { channel, .. }
                | EventKind::ChannelUserevent { channel, .. }
                | EventKind::StasisStart { channel, .. }
                | EventKind::StasisEnd { channel }
                | EventKind::ApplicationMoveFailed { channel, .. } => channel_event(channel, None),
                EventKind::ChannelEnteredBridge { channel, bridge }
                | EventKind::ChannelLeftBridge { channel, bridge } => {
                    channel_event(channel, Some(&bridge.id))
                }
                EventKind::Dial { caller, peer, .. } => {
                    if let Some(caller) = caller {
                        if caller.id != peer.id {
                            channel_event(caller, None);
                        }
                    }
                    channel_event(peer, None);
                }
                EventKind::EndpointStateChange { endpoint: value } => emit(WatchRecord {
                    timestamp: notice.timestamp.clone(),
                    entry: WatchEntry::Event {
                        event: kind.event_type().into(),
                        channel: None,
                        endpoint: Some(endpoint(value)),
                        bridge_id: None,
                    },
                }),
                EventKind::BridgeCreated { .. }
                | EventKind::BridgeDestroyed { .. }
                | EventKind::PlaybackStarted { .. }
                | EventKind::PlaybackContinuing { .. }
                | EventKind::PlaybackFinished { .. }
                | EventKind::RecordingStarted { .. }
                | EventKind::RecordingFinished { .. }
                | EventKind::RecordingFailed { .. }
                | EventKind::Published(_)
                | EventKind::ApplicationReplaced {} => {}
            }
        }
    }
}
