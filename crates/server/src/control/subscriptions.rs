use super::{CallKey, Control, ControlError, MediaOwner, Outcome};
use rusterisk_api::{ChannelId, Event, EventKind, EventSource, InvalidEventSource};
use rusterisk_core::{ResourceKind, ResourceSource, StasisApplication, SubscriptionError};

#[derive(Debug)]
pub(super) struct Notice {
    timestamp: String,
    kind: EventKind,
    source: Option<ResourceSource>,
}

#[derive(Debug)]
pub(super) enum EndedSource {
    Channel { key: CallKey },
    Bridge { id: ChannelId, generation: u64 },
}

impl EndedSource {
    fn source(&self) -> ResourceSource {
        match self {
            Self::Channel { key, .. } => specific(ResourceKind::Channel, key.generation),
            Self::Bridge { generation, .. } => specific(ResourceKind::Bridge, *generation),
        }
    }
    fn id(&self) -> &ChannelId {
        match self {
            Self::Channel { key, .. } => &key.id,
            Self::Bridge { id, .. } => id,
        }
    }
}

pub(super) enum Change {
    Subscribe,
    Unsubscribe,
}

fn specific(kind: ResourceKind, generation: u64) -> ResourceSource {
    ResourceSource::Specific { kind, generation }
}

impl Outcome {
    pub(super) fn media_notice(&mut self, owner: &MediaOwner, timestamp: &str, kind: &EventKind) {
        self.notice(timestamp, kind);
        let source = match owner {
            MediaOwner::Channel(key) => specific(ResourceKind::Channel, key.generation),
            MediaOwner::Bridge { generation, .. } => specific(ResourceKind::Bridge, *generation),
        };
        for notice in &mut self.notices {
            if notice.timestamp == timestamp && notice.kind == *kind {
                notice.source = Some(source);
            }
        }
    }

    pub(super) fn notice(&mut self, timestamp: &str, kind: &EventKind) {
        if !self
            .notices
            .iter()
            .any(|notice| notice.timestamp == timestamp && notice.kind == *kind)
        {
            self.notices.push(Notice {
                timestamp: timestamp.into(),
                kind: kind.clone(),
                source: None,
            });
        }
    }
}

impl Control {
    fn resolve_source(&self, source: &EventSource) -> Result<ResourceSource, ControlError> {
        match source {
            EventSource::AllChannels => Ok(ResourceSource::All(ResourceKind::Channel)),
            EventSource::AllBridges => Ok(ResourceSource::All(ResourceKind::Bridge)),
            EventSource::Channel(id) => self
                .channels
                .get(id)
                .map(|entry| specific(ResourceKind::Channel, entry.call.generation()))
                .ok_or(ControlError::Unprocessable("Event source does not exist")),
            EventSource::Bridge(id) => self
                .bridges
                .get(id)
                .map(|entry| specific(ResourceKind::Bridge, entry.token))
                .ok_or(ControlError::Unprocessable("Event source does not exist")),
            EventSource::AllEndpoints
            | EventSource::EndpointTechnology(_)
            | EventSource::Endpoint { .. } => Err(ControlError::Unsupported),
        }
    }

    pub(super) fn change_subscriptions(
        &mut self,
        name: &str,
        sources: Vec<String>,
        change: Change,
    ) -> Result<Outcome, ControlError> {
        let application = self
            .applications
            .get(name)
            .copied()
            .ok_or(ControlError::NotFound("Application not found"))?;
        if sources.is_empty() {
            return Err(ControlError::Invalid("Missing parameter eventSource"));
        }
        if sources.len() > 32 || sources.iter().any(|source| source.len() > 4096) {
            return Err(ControlError::Capacity);
        }
        for text in sources {
            let source = EventSource::try_from(text.as_str()).map_err(|error| match error {
                InvalidEventSource::Scheme => {
                    ControlError::Invalid("Invalid event source URI scheme")
                }
                InvalidEventSource::Resource => {
                    ControlError::Unprocessable("Event source does not exist")
                }
            })?;
            let key = self
                .resolve_source(&source)
                .map_err(|error| match (&change, error) {
                    (Change::Unsubscribe, ControlError::Unprocessable(_)) => {
                        ControlError::Unprocessable("Event source was not subscribed to")
                    }
                    (Change::Subscribe | Change::Unsubscribe, error) => error,
                })?;
            let subscriptions = self.resource_subscriptions.entry(application).or_default();
            let result = match change {
                Change::Subscribe => subscriptions.subscribe(key),
                Change::Unsubscribe => subscriptions.unsubscribe(key),
            };
            result.map_err(|error| match error {
                SubscriptionError::Capacity => ControlError::Capacity,
                SubscriptionError::Missing => {
                    ControlError::Unprocessable("Event source was not subscribed to")
                }
            })?;
        }
        Outcome::json(self.application(name)?)
    }

    pub(super) fn append_subscribed_resources(
        &self,
        application: StasisApplication,
        channels: &mut Vec<ChannelId>,
        bridges: &mut Vec<ChannelId>,
    ) -> Result<(), ControlError> {
        if let Some(subscriptions) = self.resource_subscriptions.get(&application) {
            for source in subscriptions.sources() {
                match source {
                    ResourceSource::All(ResourceKind::Channel) => {
                        channels.push(ChannelId::try_from("__AST_CHANNEL_ALL_TOPIC".to_owned())?)
                    }
                    ResourceSource::All(ResourceKind::Bridge) => {
                        bridges.push(ChannelId::try_from("__AST_BRIDGE_ALL_TOPIC".to_owned())?)
                    }
                    ResourceSource::Specific {
                        kind: ResourceKind::Channel,
                        generation,
                    } => channels.extend(
                        self.channels
                            .iter()
                            .filter(|(_, entry)| entry.call.generation() == generation)
                            .map(|(id, _)| id.clone()),
                    ),
                    ResourceSource::Specific {
                        kind: ResourceKind::Bridge,
                        generation,
                    } => bridges.extend(
                        self.bridges
                            .iter()
                            .filter(|(_, entry)| entry.token == generation)
                            .map(|(id, _)| id.clone()),
                    ),
                    ResourceSource::Specific {
                        kind: ResourceKind::Endpoint,
                        ..
                    }
                    | ResourceSource::All(ResourceKind::Endpoint) => {}
                }
            }
        }
        Ok(())
    }

    fn event_source(
        &self,
        kind: ResourceKind,
        id: &ChannelId,
        ended: &[EndedSource],
    ) -> Option<ResourceSource> {
        let active = match kind {
            ResourceKind::Channel => self
                .channels
                .get(id)
                .map(|entry| specific(kind, entry.call.generation())),
            ResourceKind::Bridge => self
                .bridges
                .get(id)
                .map(|entry| specific(kind, entry.token)),
            ResourceKind::Endpoint => None,
        };
        active.or_else(|| {
            ended
                .iter()
                .find(|source| source.source().kind() == kind && source.id() == id)
                .map(EndedSource::source)
        })
    }

    fn event_sources(
        &self,
        kind: &EventKind,
        ended: &[EndedSource],
        source: Option<ResourceSource>,
    ) -> [Option<ResourceSource>; 2] {
        if let Some(source) = source {
            return [Some(source), None];
        }
        let channel = |id| self.event_source(ResourceKind::Channel, id, ended);
        let bridge = |id| self.event_source(ResourceKind::Bridge, id, ended);
        match kind {
            EventKind::ChannelCreated { channel: value }
            | EventKind::ChannelStateChange { channel: value }
            | EventKind::ChannelCallerId { channel: value, .. }
            | EventKind::ChannelDestroyed { channel: value, .. }
            | EventKind::ChannelHangupRequest { channel: value, .. }
            | EventKind::ChannelDtmfReceived { channel: value, .. }
            | EventKind::ChannelHold { channel: value, .. }
            | EventKind::ChannelUnhold { channel: value }
            | EventKind::ChannelVarset { channel: value, .. }
            | EventKind::ChannelUserevent { channel: value, .. } => [channel(&value.id), None],
            EventKind::ChannelEnteredBridge {
                channel: value,
                bridge: mix,
            }
            | EventKind::ChannelLeftBridge {
                channel: value,
                bridge: mix,
            } => [channel(&value.id), bridge(&mix.id)],
            EventKind::BridgeCreated { bridge: value }
            | EventKind::BridgeDestroyed { bridge: value } => [bridge(&value.id), None],
            EventKind::Dial { caller, peer, .. } => [channel(&caller.id), channel(&peer.id)],
            EventKind::PlaybackStarted { .. }
            | EventKind::PlaybackContinuing { .. }
            | EventKind::PlaybackFinished { .. }
            | EventKind::RecordingStarted { .. }
            | EventKind::RecordingFinished { .. }
            | EventKind::RecordingFailed { .. } => [None, None],
            EventKind::StasisStart { .. }
            | EventKind::StasisEnd { .. }
            | EventKind::ApplicationMoveFailed { .. }
            | EventKind::ApplicationReplaced { .. } => [None, None],
        }
    }

    pub(crate) fn route_resource_events(&mut self, outcome: &mut Outcome) {
        for notice in std::mem::take(&mut outcome.notices) {
            let sources = self.event_sources(&notice.kind, &outcome.ended_sources, notice.source);
            for (name, application) in &self.applications {
                if self
                    .resource_subscriptions
                    .get(application)
                    .is_some_and(|subscriptions| {
                        sources
                            .iter()
                            .flatten()
                            .any(|source| subscriptions.interested_in(*source))
                    })
                    && !outcome.events.iter().any(|event| {
                        event.application == *name
                            && event.timestamp == notice.timestamp
                            && event.kind == notice.kind
                    })
                {
                    outcome.events.push(Event {
                        application: name.clone(),
                        timestamp: notice.timestamp.clone(),
                        kind: notice.kind.clone(),
                    });
                }
            }
        }
        for ended in outcome.ended_sources.drain(..) {
            for subscriptions in self.resource_subscriptions.values_mut() {
                subscriptions.expire(ended.source());
            }
        }
    }
}
