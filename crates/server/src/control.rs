use rusterisk_api::{Bridge, Caller, Channel, ChannelId, ChannelState, Dialplan, Event, EventKind};
use rusterisk_core::{
    AudioGate, Call, CallEvent, CallState, MediaState, MuteDirection, Transition,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlError {
    Invalid(&'static str),
    NotFound(&'static str),
    Conflict(&'static str),
    Capacity,
    Unauthorized,
    Forbidden,
    Unsupported,
    Encoding(String),
    Adapter(String),
}

impl ControlError {
    pub const fn status(&self) -> u16 {
        match self {
            Self::Invalid(_) => 400,
            Self::NotFound(_) => 404,
            Self::Conflict(_) => 409,
            Self::Capacity => 503,
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::Unsupported => 501,
            Self::Encoding(_) | Self::Adapter(_) => 500,
        }
    }
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::NotFound(message) | Self::Conflict(message) => {
                f.write_str(message)
            }
            Self::Capacity => f.write_str("Resource capacity exceeded"),
            Self::Unauthorized => f.write_str("Authentication required"),
            Self::Forbidden => f.write_str("Operation forbidden"),
            Self::Unsupported => f.write_str("Operation not supported"),
            Self::Encoding(message) | Self::Adapter(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for ControlError {}
impl From<rusterisk_api::InvalidId> for ControlError {
    fn from(_: rusterisk_api::InvalidId) -> Self {
        Self::Invalid("Invalid identifier")
    }
}
impl From<rusterisk_core::CoreError> for ControlError {
    fn from(_: rusterisk_core::CoreError) -> Self {
        Self::Conflict("Channel in invalid state")
    }
}
impl From<serde_json::Error> for ControlError {
    fn from(error: serde_json::Error) -> Self {
        Self::Encoding(error.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CallKey {
    pub id: ChannelId,
    pub generation: u64,
}

#[derive(Clone, Debug)]
pub struct IncomingCall {
    pub id: ChannelId,
    pub endpoint: String,
    pub caller: String,
    pub context: String,
    pub extension: String,
    pub app: String,
    pub args: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum Action {
    ListChannels,
    GetChannel(ChannelId),
    Originate {
        id: ChannelId,
        endpoint: String,
        app: String,
        args: Vec<String>,
        caller: String,
        timeout: u32,
    },
    Hangup(ChannelId),
    Answer(ChannelId),
    Ring(ChannelId),
    Hold(ChannelId),
    Unhold(ChannelId),
    Mute {
        channel: ChannelId,
        direction: MuteDirection,
    },
    Unmute {
        channel: ChannelId,
        direction: MuteDirection,
    },
    ListBridges,
    GetBridge(ChannelId),
    CreateBridge {
        id: ChannelId,
        name: String,
    },
    DestroyBridge(ChannelId),
    AddChannels {
        bridge: ChannelId,
        channels: Vec<ChannelId>,
    },
    RemoveChannels {
        bridge: ChannelId,
        channels: Vec<ChannelId>,
    },
}

impl Action {
    pub const fn read_only(&self) -> bool {
        match self {
            Self::ListChannels | Self::GetChannel(_) | Self::ListBridges | Self::GetBridge(_) => {
                true
            }
            Self::Originate { .. }
            | Self::Hangup(_)
            | Self::Answer(_)
            | Self::Ring(_)
            | Self::Hold(_)
            | Self::Unhold(_)
            | Self::Mute { .. }
            | Self::Unmute { .. }
            | Self::CreateBridge { .. }
            | Self::DestroyBridge(_)
            | Self::AddChannels { .. }
            | Self::RemoveChannels { .. } => false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Effect {
    Gate {
        key: CallKey,
        gate: AudioGate,
    },
    Answer {
        key: CallKey,
    },
    Ring {
        key: CallKey,
    },
    Start {
        key: CallKey,
        endpoint: String,
        caller: String,
        timeout: u32,
    },
    Release {
        key: CallKey,
    },
    Bridge {
        id: ChannelId,
        channels: Vec<CallKey>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Milestone {
    Ringing,
    Answered,
    MediaConnected,
    Ended,
}

#[derive(Clone, Debug)]
pub enum Signal {
    Milestone { key: CallKey, milestone: Milestone },
    Terminated { key: CallKey, cause: u16 },
}

#[derive(Debug)]
pub struct Outcome {
    pub status: u16,
    pub body: Option<Value>,
    pub effects: Vec<Effect>,
    pub events: Vec<Event>,
}

impl Outcome {
    pub(crate) fn empty() -> Self {
        Self {
            status: 204,
            body: None,
            effects: Vec::new(),
            events: Vec::new(),
        }
    }
    fn json(value: impl serde::Serialize) -> Result<Self, ControlError> {
        Ok(Self {
            status: 200,
            body: Some(serde_json::to_value(value)?),
            effects: Vec::new(),
            events: Vec::new(),
        })
    }
    fn event(&mut self, application: &str, timestamp: &str, kind: EventKind) {
        self.events.push(Event {
            application: application.into(),
            timestamp: timestamp.into(),
            kind,
        });
    }
    pub(crate) fn append(&mut self, mut other: Self) {
        self.effects.append(&mut other.effects);
        self.events.append(&mut other.events);
    }
}

struct ChannelEntry {
    call: Call,
    snapshot: Channel,
    application: String,
    arguments: Vec<String>,
    deadline: Option<u64>,
    stasis: Stasis,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stasis {
    Pending,
    Entered,
}

#[derive(Clone, Copy)]
enum MediaOperation {
    Hold,
    Unhold,
    Mute(MuteDirection),
    Unmute(MuteDirection),
}
struct BridgeEntry {
    token: u64,
    snapshot: Bridge,
}

pub struct Control {
    capacity: usize,
    generation: u64,
    channels: BTreeMap<ChannelId, ChannelEntry>,
    bridges: BTreeMap<ChannelId, BridgeEntry>,
    applications: BTreeSet<String>,
}

impl Control {
    pub fn incoming(
        &mut self,
        incoming: IncomingCall,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        if self.channels.contains_key(&incoming.id) {
            return Err(ControlError::Conflict(
                "Channel with given unique ID already exists",
            ));
        }
        if self.channels.len() >= self.capacity {
            return Err(ControlError::Capacity);
        }
        if !self.applications.contains(&incoming.app) {
            return Err(ControlError::NotFound("Application not found"));
        }
        ChannelId::try_from(incoming.endpoint.clone())?;
        if incoming.caller.len() > 128
            || incoming.context.is_empty()
            || incoming.context.len() > 128
            || incoming.extension.is_empty()
            || incoming.extension.len() > 128
            || incoming.args.len() > 32
            || incoming.args.iter().any(|argument| argument.len() > 256)
        {
            return Err(ControlError::Invalid("Invalid incoming channel parameters"));
        }
        let generation = self.generation()?;
        let snapshot = Channel {
            id: incoming.id.clone(),
            name: format!("PJSIP/{}-{generation:08x}", incoming.endpoint),
            state: ChannelState::Ring,
            accountcode: String::new(),
            caller: Caller {
                name: String::new(),
                number: incoming.caller,
            },
            connected: Caller::default(),
            dialplan: Dialplan {
                context: incoming.context,
                exten: incoming.extension,
                priority: 1,
                app_name: "Stasis".into(),
                app_data: std::iter::once(incoming.app.as_str())
                    .chain(incoming.args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(","),
            },
            creationtime: timestamp.into(),
            language: "en".into(),
        };
        let mut outcome = Outcome::json(&snapshot)?;
        outcome.event(
            &incoming.app,
            timestamp,
            EventKind::ChannelCreated {
                channel: snapshot.clone(),
            },
        );
        outcome.event(
            &incoming.app,
            timestamp,
            EventKind::StasisStart {
                channel: snapshot.clone(),
                args: incoming.args.clone(),
            },
        );
        self.channels.insert(
            incoming.id,
            ChannelEntry {
                call: Call::incoming(generation).apply(CallEvent::Ring).call(),
                snapshot,
                application: incoming.app,
                arguments: incoming.args,
                deadline: None,
                stasis: Stasis::Entered,
            },
        );
        Ok(outcome)
    }

    pub fn shutdown(&mut self, timestamp: &str) -> Result<Outcome, ControlError> {
        let ids: Vec<ChannelId> = self.channels.keys().cloned().collect();
        let mut outcome = Outcome::empty();
        for id in ids {
            outcome.append(self.end(&id, timestamp, 16)?);
        }
        Ok(outcome)
    }
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.min(64),
            generation: 0,
            channels: BTreeMap::new(),
            bridges: BTreeMap::new(),
            applications: BTreeSet::new(),
        }
    }

    fn generation(&mut self) -> Result<u64, ControlError> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(ControlError::Capacity)?;
        Ok(self.generation)
    }

    pub fn subscribe(&mut self, name: &str) -> Result<(), ControlError> {
        ChannelId::try_from(name.to_owned())?;
        if !self.applications.contains(name) && self.applications.len() >= 32 {
            return Err(ControlError::Capacity);
        }
        self.applications.insert(name.into());
        Ok(())
    }

    pub fn unsubscribe(&mut self, name: &str, timestamp: &str) -> Result<Outcome, ControlError> {
        self.applications.remove(name);
        let ids: Vec<ChannelId> = self
            .channels
            .iter()
            .filter(|(_, entry)| entry.application == name)
            .map(|(id, _)| id.clone())
            .collect();
        let mut outcome = Outcome::empty();
        for id in ids {
            outcome.append(self.end(&id, timestamp, 16)?);
        }
        Ok(outcome)
    }

    pub fn key(&self, id: &ChannelId) -> Result<CallKey, ControlError> {
        let entry = self
            .channels
            .get(id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        Ok(CallKey {
            id: id.clone(),
            generation: entry.call.generation(),
        })
    }
    pub fn channel(&self, id: &ChannelId) -> Result<&Channel, ControlError> {
        self.channels
            .get(id)
            .map(|entry| &entry.snapshot)
            .ok_or(ControlError::NotFound("Channel not found"))
    }
    pub fn bridge(&self, id: &ChannelId) -> Result<&Bridge, ControlError> {
        self.bridges
            .get(id)
            .map(|entry| &entry.snapshot)
            .ok_or(ControlError::NotFound("Bridge not found"))
    }

    pub fn apply(
        &mut self,
        action: Action,
        timestamp: &str,
        now_ms: u64,
    ) -> Result<Outcome, ControlError> {
        match action {
            Action::ListChannels => Outcome::json(
                self.channels
                    .values()
                    .map(|entry| &entry.snapshot)
                    .collect::<Vec<_>>(),
            ),
            Action::GetChannel(id) => Outcome::json(self.channel(&id)?),
            Action::Originate {
                id,
                endpoint,
                app,
                args,
                caller,
                timeout,
            } => {
                if self.channels.contains_key(&id) {
                    return Err(ControlError::Conflict(
                        "Channel with given unique ID already exists",
                    ));
                }
                if self.channels.len() >= self.capacity {
                    return Err(ControlError::Capacity);
                }
                if !self.applications.contains(&app) {
                    return Err(ControlError::NotFound("Application not found"));
                }
                if endpoint.len() > 256
                    || !endpoint.starts_with("PJSIP/")
                    || endpoint.len() <= 6
                    || caller.len() > 128
                    || timeout == 0
                    || timeout > 3600
                    || args.len() > 32
                    || args.iter().any(|value| value.len() > 256)
                {
                    return Err(ControlError::Invalid(
                        "Invalid parameters for originating a channel",
                    ));
                }
                let generation = self.generation()?;
                let deadline = now_ms
                    .checked_add(u64::from(timeout) * 1000)
                    .ok_or(ControlError::Capacity)?;
                let snapshot = Channel {
                    id: id.clone(),
                    name: format!("{endpoint}-{generation:08x}"),
                    state: ChannelState::Down,
                    accountcode: String::new(),
                    caller: Caller {
                        name: String::new(),
                        number: caller.clone(),
                    },
                    connected: Caller::default(),
                    dialplan: Dialplan {
                        context: "default".into(),
                        exten: "s".into(),
                        priority: 1,
                        app_name: String::new(),
                        app_data: String::new(),
                    },
                    creationtime: timestamp.into(),
                    language: "en".into(),
                };
                let mut outcome = Outcome::json(&snapshot)?;
                outcome.event(
                    &app,
                    timestamp,
                    EventKind::ChannelCreated {
                        channel: snapshot.clone(),
                    },
                );
                outcome.effects.push(Effect::Start {
                    key: CallKey {
                        id: id.clone(),
                        generation,
                    },
                    endpoint,
                    caller,
                    timeout,
                });
                self.channels.insert(
                    id,
                    ChannelEntry {
                        call: Call::outgoing(generation),
                        snapshot,
                        application: app,
                        arguments: args,
                        deadline: Some(deadline),
                        stasis: Stasis::Pending,
                    },
                );
                Ok(outcome)
            }
            Action::Hangup(id) => self.end(&id, timestamp, 16),
            Action::Hold(id) => self.media(&id, MediaOperation::Hold, timestamp),
            Action::Unhold(id) => self.media(&id, MediaOperation::Unhold, timestamp),
            Action::Mute { channel, direction } => {
                self.media(&channel, MediaOperation::Mute(direction), timestamp)
            }
            Action::Unmute { channel, direction } => {
                self.media(&channel, MediaOperation::Unmute(direction), timestamp)
            }
            Action::Answer(id) => {
                let entry = self
                    .channels
                    .get(&id)
                    .ok_or(ControlError::NotFound("Channel not found"))?;
                if entry.stasis != Stasis::Entered {
                    return Err(ControlError::Conflict("Channel not in Stasis application"));
                }
                let mut outcome = Outcome::empty();
                if entry.call.state() != CallState::Up {
                    outcome.effects.push(Effect::Answer {
                        key: self.key(&id)?,
                    });
                }
                Ok(outcome)
            }
            Action::Ring(id) => {
                let entry = self
                    .channels
                    .get(&id)
                    .ok_or(ControlError::NotFound("Channel not found"))?;
                if entry.stasis != Stasis::Entered {
                    return Err(ControlError::Conflict("Channel not in Stasis application"));
                }
                let mut outcome = Outcome::empty();
                if entry.call.state() != CallState::Up {
                    outcome.effects.push(Effect::Ring {
                        key: self.key(&id)?,
                    });
                }
                Ok(outcome)
            }
            Action::ListBridges => Outcome::json(
                self.bridges
                    .values()
                    .map(|entry| &entry.snapshot)
                    .collect::<Vec<_>>(),
            ),
            Action::GetBridge(id) => Outcome::json(self.bridge(&id)?),
            Action::CreateBridge { id, name } => {
                if self.bridges.contains_key(&id) {
                    return Err(ControlError::Conflict("Bridge already exists"));
                }
                if self.bridges.len() >= self.capacity || name.len() > 128 {
                    return Err(ControlError::Capacity);
                }
                let token = self.generation()?;
                let snapshot = Bridge {
                    id: id.clone(),
                    technology: "simple_bridge".into(),
                    bridge_type: "mixing".into(),
                    bridge_class: "stasis".into(),
                    creator: "Stasis".into(),
                    name,
                    channels: Vec::new(),
                    creationtime: timestamp.into(),
                    video_mode: "none".into(),
                };
                let mut outcome = Outcome::json(&snapshot)?;
                for app in &self.applications {
                    outcome.event(
                        app,
                        timestamp,
                        EventKind::BridgeCreated {
                            bridge: snapshot.clone(),
                        },
                    );
                }
                self.bridges.insert(id, BridgeEntry { token, snapshot });
                Ok(outcome)
            }
            Action::DestroyBridge(id) => {
                let channels = self.bridge(&id)?.channels.clone();
                let mut outcome = self.membership(id.clone(), channels, false, timestamp)?;
                let bridge = self
                    .bridges
                    .remove(&id)
                    .ok_or(ControlError::NotFound("Bridge not found"))?;
                for app in &self.applications {
                    outcome.event(
                        app,
                        timestamp,
                        EventKind::BridgeDestroyed {
                            bridge: bridge.snapshot.clone(),
                        },
                    );
                }
                Ok(outcome)
            }
            Action::AddChannels { bridge, channels } => {
                self.membership(bridge, channels, true, timestamp)
            }
            Action::RemoveChannels { bridge, channels } => {
                self.membership(bridge, channels, false, timestamp)
            }
        }
    }

    fn membership(
        &mut self,
        id: ChannelId,
        channels: Vec<ChannelId>,
        adding: bool,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let bridge = self
            .bridges
            .get(&id)
            .ok_or(ControlError::NotFound("Bridge not found"))?;
        if channels.len() > self.capacity
            || channels.iter().collect::<BTreeSet<_>>().len() != channels.len()
        {
            return Err(ControlError::Invalid("Invalid channel list"));
        }
        let token = bridge.token;
        let mut updates = Vec::new();
        for channel in &channels {
            let entry = self
                .channels
                .get(channel)
                .ok_or(ControlError::NotFound("Channel not found"))?;
            let call = if adding {
                entry.call.join(token)?
            } else {
                entry.call.leave(token)?
            };
            updates.push((channel.clone(), call));
        }
        let bridge = self
            .bridges
            .get_mut(&id)
            .ok_or(ControlError::NotFound("Bridge not found"))?;
        for (channel, _) in &updates {
            if adding {
                bridge.snapshot.channels.push(channel.clone());
            } else {
                bridge.snapshot.channels.retain(|value| value != channel);
            }
        }
        let snapshot = bridge.snapshot.clone();
        let mut outcome = Outcome::empty();
        for (channel, call) in updates {
            let entry = self
                .channels
                .get_mut(&channel)
                .ok_or(ControlError::NotFound("Channel not found"))?;
            entry.call = call;
            let kind = if adding {
                EventKind::ChannelEnteredBridge {
                    bridge: snapshot.clone(),
                    channel: entry.snapshot.clone(),
                }
            } else {
                EventKind::ChannelLeftBridge {
                    bridge: snapshot.clone(),
                    channel: entry.snapshot.clone(),
                }
            };
            outcome.event(&entry.application, timestamp, kind);
        }
        let keys = snapshot
            .channels
            .iter()
            .map(|id| self.key(id))
            .collect::<Result<Vec<_>, _>>()?;
        outcome.effects.push(Effect::Bridge { id, channels: keys });
        Ok(outcome)
    }

    fn end(
        &mut self,
        id: &ChannelId,
        timestamp: &str,
        cause: u16,
    ) -> Result<Outcome, ControlError> {
        let mut entry = self
            .channels
            .remove(id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        let mut outcome = Outcome::empty();
        if let Some(token) = entry.call.bridge() {
            for bridge in self
                .bridges
                .values_mut()
                .filter(|bridge| bridge.token == token)
            {
                bridge.snapshot.channels.retain(|value| value != id);
                outcome.event(
                    &entry.application,
                    timestamp,
                    EventKind::ChannelLeftBridge {
                        bridge: bridge.snapshot.clone(),
                        channel: entry.snapshot.clone(),
                    },
                );
                let keys = bridge
                    .snapshot
                    .channels
                    .iter()
                    .filter_map(|id| {
                        self.channels.get(id).map(|entry| CallKey {
                            id: id.clone(),
                            generation: entry.call.generation(),
                        })
                    })
                    .collect();
                outcome.effects.push(Effect::Bridge {
                    id: bridge.snapshot.id.clone(),
                    channels: keys,
                });
            }
        }
        entry.call = entry.call.apply(CallEvent::Hangup).call();
        outcome.effects.push(Effect::Release {
            key: CallKey {
                id: id.clone(),
                generation: entry.call.generation(),
            },
        });
        if entry.stasis == Stasis::Entered {
            outcome.event(
                &entry.application,
                timestamp,
                EventKind::StasisEnd {
                    channel: entry.snapshot.clone(),
                },
            );
        }
        outcome.event(
            &entry.application,
            timestamp,
            EventKind::ChannelDestroyed {
                cause,
                cause_txt: match cause {
                    16 => "Normal Clearing",
                    17 => "User busy",
                    19 => "User alerting, no answer",
                    21 => "Call Rejected",
                    102 => "Recover on timer expiry",
                    _ => "Unknown",
                }
                .into(),
                channel: entry.snapshot,
            },
        );
        Ok(outcome)
    }

    fn media(
        &mut self,
        id: &ChannelId,
        operation: MediaOperation,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let entry = self
            .channels
            .get_mut(id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        if entry.stasis != Stasis::Entered {
            return Err(ControlError::Conflict("Channel not in Stasis application"));
        }
        let mut outcome = Outcome::empty();
        let call = match operation {
            MediaOperation::Hold if entry.call.media() == MediaState::Held => return Ok(outcome),
            MediaOperation::Unhold if entry.call.media() == MediaState::Active => {
                return Ok(outcome)
            }
            MediaOperation::Hold => entry.call.hold()?,
            MediaOperation::Unhold => entry.call.resume()?,
            MediaOperation::Mute(direction) => entry.call.mute(direction)?,
            MediaOperation::Unmute(direction) => entry.call.unmute(direction)?,
        };
        if entry.call == call {
            return Ok(outcome);
        }
        entry.call = call;
        if call.state() == CallState::Up {
            outcome.effects.push(Effect::Gate {
                key: CallKey {
                    id: id.clone(),
                    generation: call.generation(),
                },
                gate: call.audio_gate(),
            });
        }
        match operation {
            MediaOperation::Hold => outcome.event(
                &entry.application,
                timestamp,
                EventKind::ChannelHold {
                    channel: entry.snapshot.clone(),
                },
            ),
            MediaOperation::Unhold => outcome.event(
                &entry.application,
                timestamp,
                EventKind::ChannelUnhold {
                    channel: entry.snapshot.clone(),
                },
            ),
            MediaOperation::Mute(_) | MediaOperation::Unmute(_) => {}
        }
        Ok(outcome)
    }

    pub fn signal(&mut self, signal: Signal, timestamp: &str) -> Result<Outcome, ControlError> {
        match signal {
            Signal::Terminated { key, cause } => {
                if self
                    .channels
                    .get(&key.id)
                    .is_some_and(|entry| entry.call.generation() == key.generation)
                {
                    self.end(&key.id, timestamp, cause)
                } else {
                    let mut outcome = Outcome::empty();
                    outcome.effects.push(Effect::Release { key });
                    Ok(outcome)
                }
            }
            Signal::Milestone { key, milestone } => {
                let Some(entry) = self.channels.get(&key.id) else {
                    let mut outcome = Outcome::empty();
                    outcome.effects.push(Effect::Release { key });
                    return Ok(outcome);
                };
                if entry.call.generation() != key.generation {
                    let mut outcome = Outcome::empty();
                    outcome.effects.push(Effect::Release { key });
                    return Ok(outcome);
                }
                if milestone == Milestone::Ended {
                    return self.end(&key.id, timestamp, 16);
                }
                let entry = self
                    .channels
                    .get_mut(&key.id)
                    .ok_or(ControlError::NotFound("Channel not found"))?;
                let mut outcome = Outcome::empty();
                match milestone {
                    Milestone::Ringing | Milestone::Answered => {
                        let before = entry.call.state();
                        entry.call = entry
                            .call
                            .apply(if milestone == Milestone::Ringing {
                                CallEvent::Ring
                            } else {
                                CallEvent::Answer
                            })
                            .call();
                        entry.snapshot.state = match entry.call.state() {
                            CallState::Down => ChannelState::Down,
                            CallState::Ringing => ChannelState::Ringing,
                            CallState::Up => ChannelState::Up,
                            CallState::Ended => ChannelState::Down,
                        };
                        if entry.call.state() != before {
                            if entry.call.state() == CallState::Up
                                && entry.call.audio_gate() != AudioGate::Open
                            {
                                outcome.effects.push(Effect::Gate {
                                    key: key.clone(),
                                    gate: entry.call.audio_gate(),
                                });
                            }
                            outcome.event(
                                &entry.application,
                                timestamp,
                                EventKind::ChannelStateChange {
                                    channel: entry.snapshot.clone(),
                                },
                            );
                            if entry.call.state() == CallState::Up
                                && entry.stasis == Stasis::Pending
                            {
                                entry.stasis = Stasis::Entered;
                                entry.deadline = None;
                                entry.snapshot.dialplan.app_name = "Stasis".into();
                                entry.snapshot.dialplan.app_data = entry.application.clone();
                                outcome.event(
                                    &entry.application,
                                    timestamp,
                                    EventKind::StasisStart {
                                        args: entry.arguments.clone(),
                                        channel: entry.snapshot.clone(),
                                    },
                                );
                            }
                        }
                    }
                    Milestone::MediaConnected => {
                        let decision = entry.call.connect_media();
                        entry.call = decision.call();
                        if decision.transition() == Transition::Notify {
                            outcome.event(
                                &entry.application,
                                timestamp,
                                EventKind::ChannelVarset {
                                    variable: "RUSTERISK_MEDIA_CONNECTED".into(),
                                    value: "1".into(),
                                    channel: entry.snapshot.clone(),
                                },
                            );
                        }
                    }
                    Milestone::Ended => {
                        return Err(ControlError::Conflict("Terminal event was already handled"))
                    }
                }
                Ok(outcome)
            }
        }
    }

    pub fn expire(&mut self, now_ms: u64, timestamp: &str) -> Result<Outcome, ControlError> {
        let expired: Vec<ChannelId> = self
            .channels
            .iter()
            .filter(|(_, entry)| entry.deadline.is_some_and(|deadline| deadline <= now_ms))
            .map(|(id, _)| id.clone())
            .collect();
        let mut outcome = Outcome::empty();
        for id in expired {
            outcome.append(self.end(&id, timestamp, 19)?);
        }
        Ok(outcome)
    }
}
