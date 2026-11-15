use crate::media::{PlaybackControl, RecordingEnd};
use rusterisk_api::{
    Bridge, Caller, Channel, ChannelId, ChannelState, Dialplan, Event, EventKind, MediaUri,
    RecordingOptions,
};
use rusterisk_core::{
    AudioGate, Call, CallEvent, CallState, ContactPresence, DtmfCommand, DtmfDigit, DtmfError,
    DtmfOutbox, DtmfTiming, DtmfToken, MediaConnection, MediaState, MediaTaskError, MediaTaskToken,
    MediaTasks, MuteDirection, PeerMediaDirection, RecordingControl, StasisApplication,
    StasisMembership, Transition,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
mod dial;
mod dialplan;
mod endpoints;
mod playback;
mod recording;
mod stasis;
mod subscriptions;
use dialplan::DialplanFrame;
pub use endpoints::EndpointKey;
use endpoints::Endpoints;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlError {
    Invalid(&'static str),
    NotFound(&'static str),
    EndpointTechnology(String),
    BridgeExists(ChannelId),
    Conflict(&'static str),
    InvalidState(&'static str),
    Unprocessable(&'static str),
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
            Self::NotFound(_) | Self::EndpointTechnology(_) => 404,
            Self::Conflict(_) | Self::BridgeExists(_) => 409,
            Self::InvalidState(_) => 412,
            Self::Unprocessable(_) => 422,
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
            Self::Invalid(message)
            | Self::NotFound(message)
            | Self::Conflict(message)
            | Self::Unprocessable(message)
            | Self::InvalidState(message) => f.write_str(message),
            Self::EndpointTechnology(technology) => {
                write!(f, "No Endpoints found - invalid tech {technology}")
            }
            Self::BridgeExists(id) => write!(f, "Bridge with id '{id}' already exists"),
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

impl From<DtmfError> for ControlError {
    fn from(error: DtmfError) -> Self {
        match error {
            DtmfError::Empty => Self::Invalid("DTMF is required"),
            DtmfError::Timing => Self::Invalid("Invalid digit timing"),
            DtmfError::Capacity => Self::Capacity,
            DtmfError::Clock => Self::Adapter("Digit timer overflow".into()),
            DtmfError::Unsupported => Self::Unsupported,
            DtmfError::Ended => Self::InvalidState("Channel in invalid state"),
        }
    }
}
impl From<MediaTaskError> for ControlError {
    fn from(error: MediaTaskError) -> Self {
        match error {
            MediaTaskError::Capacity => Self::Capacity,
            MediaTaskError::Ended => Self::InvalidState("Media owner ended"),
            MediaTaskError::Unknown
            | MediaTaskError::Order
            | MediaTaskError::State
            | MediaTaskError::Recording => Self::Conflict("Media task in invalid state"),
        }
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaOwner {
    Channel(CallKey),
    Bridge { id: ChannelId, generation: u64 },
}

#[derive(Clone, Debug)]
pub struct IncomingCall {
    pub id: ChannelId,
    pub endpoint: String,
    pub caller: String,
    pub context: String,
    pub extension: String,
    pub target: IncomingTarget,
}

#[derive(Clone, Debug)]
pub enum IncomingTarget {
    Dialplan,
    Stasis {
        application: String,
        arguments: Vec<String>,
    },
}

#[derive(Clone, Debug)]
pub enum Action {
    RecordChannel {
        channel: ChannelId,
        name: ChannelId,
        options: RecordingOptions,
    },
    RecordBridge {
        bridge: ChannelId,
        name: ChannelId,
        options: RecordingOptions,
    },
    GetRecording(ChannelId),
    ControlRecording {
        name: ChannelId,
        operation: RecordingControl,
    },
    EndRecording {
        name: ChannelId,
        mode: RecordingEnd,
    },
    PlayBridge {
        bridge: ChannelId,
        id: ChannelId,
        media: MediaUri,
    },
    PlayChannel {
        channel: ChannelId,
        id: ChannelId,
        media: MediaUri,
    },
    GetPlayback(ChannelId),
    ControlPlayback {
        id: ChannelId,
        operation: PlaybackControl,
    },
    SendDtmf {
        channel: ChannelId,
        digits: String,
        timing: DtmfTiming,
    },
    ListChannels,
    GetChannel(ChannelId),
    ListApplications,
    GetApplication(String),
    FilterApplication {
        application: String,
        update: Option<rusterisk_api::EventFilterRequest>,
    },
    SubscribeResources {
        application: String,
        sources: Vec<String>,
    },
    UnsubscribeResources {
        application: String,
        sources: Vec<String>,
    },
    ListEndpoints {
        technology: Option<String>,
    },
    GetEndpoint {
        technology: String,
        resource: String,
    },
    GetVariable {
        channel: ChannelId,
        variable: String,
    },
    SetVariable {
        channel: ChannelId,
        variable: String,
        value: Option<String>,
    },
    Originate {
        id: ChannelId,
        endpoint: String,
        app: String,
        args: Vec<String>,
        caller: String,
        timeout: u32,
    },
    Hangup(ChannelId),
    Continue {
        channel: ChannelId,
        context: Option<String>,
        extension: Option<String>,
        priority: Option<u16>,
    },
    Move {
        channel: ChannelId,
        app: String,
        args: Vec<String>,
    },
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
    pub(crate) const fn effect_budget(&self) -> usize {
        match self {
            Self::ListChannels
            | Self::ListApplications
            | Self::GetApplication(_)
            | Self::FilterApplication { .. }
            | Self::SubscribeResources { .. }
            | Self::UnsubscribeResources { .. }
            | Self::ListEndpoints { .. }
            | Self::GetEndpoint { .. }
            | Self::GetRecording(_)
            | Self::GetPlayback(_)
            | Self::GetChannel(_)
            | Self::GetVariable { .. }
            | Self::SetVariable { .. }
            | Self::ListBridges
            | Self::GetBridge(_)
            | Self::CreateBridge { .. }
            | Self::Move { .. } => 0,
            Self::Hangup(_) | Self::Continue { .. } | Self::DestroyBridge(_) => 2,
            Self::Originate { .. }
            | Self::RecordChannel { .. }
            | Self::RecordBridge { .. }
            | Self::ControlRecording { .. }
            | Self::EndRecording { .. }
            | Self::PlayBridge { .. }
            | Self::PlayChannel { .. }
            | Self::ControlPlayback { .. }
            | Self::SendDtmf { .. }
            | Self::Answer(_)
            | Self::Ring(_)
            | Self::Hold(_)
            | Self::Unhold(_)
            | Self::Mute { .. }
            | Self::Unmute { .. }
            | Self::AddChannels { .. }
            | Self::RemoveChannels { .. } => 1,
        }
    }

    pub const fn read_only(&self) -> bool {
        match self {
            Self::ListChannels
            | Self::ListApplications
            | Self::GetApplication(_)
            | Self::ListEndpoints { .. }
            | Self::GetEndpoint { .. }
            | Self::GetRecording(_)
            | Self::GetPlayback(_)
            | Self::GetChannel(_)
            | Self::GetVariable { .. }
            | Self::ListBridges
            | Self::GetBridge(_) => true,
            Self::Originate { .. }
            | Self::FilterApplication { .. }
            | Self::SubscribeResources { .. }
            | Self::UnsubscribeResources { .. }
            | Self::RecordChannel { .. }
            | Self::RecordBridge { .. }
            | Self::ControlRecording { .. }
            | Self::EndRecording { .. }
            | Self::PlayBridge { .. }
            | Self::PlayChannel { .. }
            | Self::ControlPlayback { .. }
            | Self::SendDtmf { .. }
            | Self::SetVariable { .. }
            | Self::Hangup(_)
            | Self::Continue { .. }
            | Self::Move { .. }
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
    RecordMedia {
        owner: MediaOwner,
        token: MediaTaskToken,
        name: ChannelId,
        options: RecordingOptions,
    },
    ControlRecording {
        token: MediaTaskToken,
        operation: RecordingControl,
    },
    EndRecording {
        token: MediaTaskToken,
        mode: RecordingEnd,
    },
    PlayMedia {
        owner: MediaOwner,
        token: MediaTaskToken,
        media: MediaUri,
    },
    ControlPlayback {
        token: MediaTaskToken,
        operation: PlaybackControl,
    },
    Playback {
        key: CallKey,
        token: MediaTaskToken,
        media: String,
    },
    Dtmf {
        key: CallKey,
        command: DtmfCommand,
    },
    Gate {
        key: CallKey,
        gate: AudioGate,
    },
    PeerDirection {
        key: CallKey,
        direction: PeerMediaDirection,
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
        timeout: Option<u32>,
    },
    Release {
        key: CallKey,
    },
    Bridge {
        id: ChannelId,
        generation: u64,
        channels: Vec<CallKey>,
    },
    ReleaseBridge {
        generation: u64,
    },
    DialBridge {
        parent: CallKey,
        generation: u64,
        peer: Option<CallKey>,
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
    EndpointContact {
        key: EndpointKey,
        presence: ContactPresence,
    },
    PeerDirection {
        key: CallKey,
        direction: PeerMediaDirection,
    },
    RecordingAdmitted {
        owner: MediaOwner,
        token: MediaTaskToken,
        result: Result<(), ControlError>,
    },
    RecordingFinished {
        owner: MediaOwner,
        token: MediaTaskToken,
        result: Result<(), ControlError>,
    },
    MediaPlaybackStarted {
        owner: MediaOwner,
        token: MediaTaskToken,
    },
    MediaPlaybackFinished {
        owner: MediaOwner,
        token: MediaTaskToken,
        completion: PlaybackCompletion,
    },
    PlaybackStarted {
        key: CallKey,
        token: MediaTaskToken,
    },
    PlaybackFinished {
        key: CallKey,
        token: MediaTaskToken,
        completion: PlaybackCompletion,
    },
    DtmfReady {
        key: CallKey,
        events: u16,
    },
    DtmfFinished {
        key: CallKey,
        token: DtmfToken,
    },
    DtmfReceived {
        key: CallKey,
        digit: rusterisk_core::DtmfDigit,
        duration_ms: u32,
    },
    Milestone {
        key: CallKey,
        milestone: Milestone,
    },
    Terminated {
        key: CallKey,
        cause: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackCompletion {
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallHistoryEvent {
    Started { key: CallKey, timestamp: String },
    Finished { key: CallKey, timestamp: String },
}

#[derive(Debug)]
pub struct Outcome {
    pub status: u16,
    pub location: Option<String>,
    pub body: Option<Value>,
    pub effects: Vec<Effect>,
    pub events: Vec<Event>,
    pub history: Vec<CallHistoryEvent>,
    notices: Vec<subscriptions::Notice>,
    ended_sources: Vec<subscriptions::EndedSource>,
}

impl Outcome {
    pub(crate) fn empty() -> Self {
        Self {
            status: 204,
            location: None,
            body: None,
            effects: Vec::new(),
            events: Vec::new(),
            history: Vec::new(),
            notices: Vec::new(),
            ended_sources: Vec::new(),
        }
    }
    pub(crate) fn json(value: impl serde::Serialize) -> Result<Self, ControlError> {
        Ok(Self {
            status: 200,
            location: None,
            body: Some(serde_json::to_value(value)?),
            effects: Vec::new(),
            events: Vec::new(),
            history: Vec::new(),
            notices: Vec::new(),
            ended_sources: Vec::new(),
        })
    }
    fn event(&mut self, application: &str, timestamp: &str, kind: EventKind) {
        self.notice(timestamp, &kind);
        self.events.push(Event {
            application: application.into(),
            timestamp: timestamp.into(),
            kind,
        });
    }
    fn optional_event(&mut self, application: Option<&str>, timestamp: &str, kind: EventKind) {
        if let Some(application) = application {
            self.event(application, timestamp, kind);
        } else {
            self.notice(timestamp, &kind);
        }
    }

    pub(crate) fn append(&mut self, mut other: Self) {
        self.effects.append(&mut other.effects);
        self.events.append(&mut other.events);
        self.history.append(&mut other.history);
        self.notices.append(&mut other.notices);
        self.ended_sources.append(&mut other.ended_sources);
    }
}

struct ChannelEntry {
    call: Call,
    endpoint: String,
    snapshot: Channel,
    arguments: Vec<String>,
    deadline: Option<u64>,
    variables: BTreeMap<String, String>,
    dtmf: Option<DtmfOutbox>,
    dialplan: Option<DialplanFrame>,
    media_tasks: MediaTasks,
}

impl ChannelEntry {
    fn variable(&self, name: &str) -> Result<String, ControlError> {
        let value = match name {
            "EXTEN" => self.snapshot.dialplan.exten.clone(),
            "CONTEXT" => self.snapshot.dialplan.context.clone(),
            "PRIORITY" => self.snapshot.dialplan.priority.to_string(),
            "CALLERID(num)" => self.snapshot.caller.number.clone(),
            "CALLERID(name)" => self.snapshot.caller.name.clone(),
            "CALLERID(all)" => format!(
                "\"{}\" <{}>",
                self.snapshot.caller.name, self.snapshot.caller.number
            ),
            "RUSTERISK_MEDIA_CONNECTED" => match self.call.media_connection() {
                MediaConnection::Connected => "1".into(),
                MediaConnection::Pending | MediaConnection::Released => "0".into(),
            },
            _ => {
                validate_variable(name)?;
                self.variables
                    .get(name)
                    .cloned()
                    .ok_or(ControlError::NotFound("Provided variable was not found"))?
            }
        };
        Ok(value)
    }
}

fn validate_variable(name: &str) -> Result<(), ControlError> {
    if name.is_empty() || name.len() > 128 {
        return Err(ControlError::Invalid("Invalid variable name"));
    }
    if name.contains(['(', ')']) {
        return Err(ControlError::Unsupported);
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(ControlError::Invalid("Invalid variable name"));
    }
    Ok(())
}

fn application_name(
    applications: &BTreeMap<String, StasisApplication>,
    membership: StasisMembership,
) -> Result<Option<&str>, ControlError> {
    match membership {
        StasisMembership::Outside | StasisMembership::Released => Ok(None),
        StasisMembership::Pending(application) | StasisMembership::Entered(application) => {
            applications
                .iter()
                .find_map(|(name, current)| (*current == application).then_some(name.as_str()))
                .map(Some)
                .ok_or_else(|| {
                    ControlError::Adapter("Stasis application ownership is missing".into())
                })
        }
    }
}

#[derive(Clone, Copy)]
enum MediaOperation {
    Hold,
    Unhold,
    Mute(MuteDirection),
    Unmute(MuteDirection),
}

enum CallerField {
    Name,
    Number,
    All,
}

#[derive(Clone, Copy)]
enum VariableAccess {
    Stasis,
    Dialplan,
}

impl VariableAccess {
    fn check(self, membership: StasisMembership) -> Result<(), ControlError> {
        match self {
            Self::Stasis if !matches!(membership, StasisMembership::Entered(_)) => {
                Err(ControlError::Conflict("Channel not in Stasis application"))
            }
            Self::Stasis | Self::Dialplan => Ok(()),
        }
    }
}
struct BridgeEntry {
    token: u64,
    snapshot: Bridge,
    media_tasks: MediaTasks,
}

pub struct Control {
    capacity: usize,
    generation: u64,
    channels: BTreeMap<ChannelId, ChannelEntry>,
    bridges: BTreeMap<ChannelId, BridgeEntry>,
    applications: BTreeMap<String, StasisApplication>,
    event_filters: BTreeMap<StasisApplication, stasis::EventFilters>,
    resource_subscriptions: BTreeMap<StasisApplication, rusterisk_core::ResourceSubscriptions<128>>,
    playbacks: BTreeMap<ChannelId, playback::PlaybackEntry>,
    recordings: BTreeMap<ChannelId, recording::RecordingEntry>,
    dialplan_program: rusterisk_config::Dialplan,
    endpoints: Endpoints,
}

impl Control {
    pub(crate) fn terminal_effect_reserve(&self) -> usize {
        self.channels.len() * 2
    }

    pub(crate) fn effect_budget(&self, action: &Action) -> usize {
        if let Action::Hangup(id) = action {
            self.termination_budget(id)
        } else {
            action.effect_budget()
        }
    }

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
        let (application, arguments, app_name, app_data) = match incoming.target {
            IncomingTarget::Dialplan => (None, Vec::new(), String::new(), String::new()),
            IncomingTarget::Stasis {
                application,
                arguments,
            } => {
                let token = self
                    .applications
                    .get(&application)
                    .copied()
                    .ok_or(ControlError::NotFound("Application not found"))?;
                if arguments.len() > 32 || arguments.iter().any(|argument| argument.len() > 256) {
                    return Err(ControlError::Invalid("Invalid incoming channel parameters"));
                }
                let data = std::iter::once(application.as_str())
                    .chain(arguments.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(",");
                (Some(token), arguments, "Stasis".into(), data)
            }
        };
        ChannelId::try_from(incoming.endpoint.clone())?;
        if incoming.caller.len() > 128
            || incoming.context.is_empty()
            || incoming.context.len() > 128
            || incoming.extension.is_empty()
            || incoming.extension.len() > 128
        {
            return Err(ControlError::Invalid("Invalid incoming channel parameters"));
        }
        let generation = self.generation()?;
        let mut call = Call::incoming(generation).apply(CallEvent::Ring).call();
        if let Some(application) = application {
            call = call.enter_stasis(application)?;
        }
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
                app_name,
                app_data,
            },
            creationtime: timestamp.into(),
            language: "en".into(),
            channelvars: None,
        };
        let mut outcome = Outcome::json(&snapshot)?;
        outcome.history.push(CallHistoryEvent::Started {
            key: CallKey {
                id: incoming.id.clone(),
                generation,
            },
            timestamp: timestamp.into(),
        });
        outcome.optional_event(
            application_name(&self.applications, call.stasis())?,
            timestamp,
            EventKind::ChannelCreated {
                channel: snapshot.clone(),
            },
        );
        if matches!(call.stasis(), StasisMembership::Entered(_)) {
            outcome.optional_event(
                application_name(&self.applications, call.stasis())?,
                timestamp,
                EventKind::StasisStart {
                    channel: snapshot.clone(),
                    args: arguments.clone(),
                },
            );
        }
        self.channels.insert(
            incoming.id,
            ChannelEntry {
                call,
                endpoint: incoming.endpoint,
                snapshot,
                arguments,
                deadline: None,
                variables: BTreeMap::new(),
                dtmf: None,
                dialplan: Some(DialplanFrame::dormant(self.dialplan_program.clone())),
                media_tasks: MediaTasks::new(call.generation()),
            },
        );
        Ok(outcome)
    }

    pub fn shutdown(&mut self, timestamp: &str) -> Result<Outcome, ControlError> {
        let ids: Vec<ChannelId> = self.channels.keys().cloned().collect();
        let mut outcome = Outcome::empty();
        for id in ids {
            if self.channels.contains_key(&id) {
                outcome.append(self.end(&id, timestamp, 16)?);
            }
        }
        Ok(outcome)
    }
    pub fn new(capacity: usize) -> Self {
        Self::with_dialplan(capacity, rusterisk_config::Dialplan { steps: Vec::new() })
    }

    pub fn with_configuration(capacity: usize, config: &rusterisk_config::Config) -> Self {
        let mut control = Self::with_dialplan(capacity, config.dialplan.clone());
        control.endpoints = Endpoints::configured(config);
        control
    }

    pub fn with_dialplan(capacity: usize, program: rusterisk_config::Dialplan) -> Self {
        Self {
            capacity: capacity.min(64),
            generation: 0,
            channels: BTreeMap::new(),
            bridges: BTreeMap::new(),
            applications: BTreeMap::new(),
            event_filters: BTreeMap::new(),
            resource_subscriptions: BTreeMap::new(),
            playbacks: BTreeMap::new(),
            recordings: BTreeMap::new(),
            dialplan_program: program,
            endpoints: Endpoints::default(),
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
        if self.applications.contains_key(name) {
            return Ok(());
        }
        if self.applications.len() >= 32 {
            return Err(ControlError::Capacity);
        }
        let application = StasisApplication::new(self.generation()?);
        self.applications.insert(name.into(), application);
        Ok(())
    }

    pub fn unsubscribe(&mut self, name: &str, timestamp: &str) -> Result<Outcome, ControlError> {
        let Some(application) = self.applications.get(name).copied() else {
            return Ok(Outcome::empty());
        };
        let ids: Vec<ChannelId> = self.channels.iter().filter(|(_, entry)| matches!(entry.call.stasis(), StasisMembership::Pending(current) | StasisMembership::Entered(current) if current == application)).map(|(id, _)| id.clone()).collect();
        let mut outcome = Outcome::empty();
        for id in ids {
            if self.channels.contains_key(&id) {
                outcome.append(self.end(&id, timestamp, 16)?);
            }
        }
        self.applications.remove(name);
        self.event_filters.remove(&application);
        self.resource_subscriptions.remove(&application);
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
            Action::RecordChannel {
                channel,
                name,
                options,
            } => self.record_channel(channel, name, options),
            Action::RecordBridge {
                bridge,
                name,
                options,
            } => self.record_bridge(bridge, name, options),
            Action::GetRecording(name) => self.get_recording(&name),
            Action::ControlRecording { name, operation } => {
                self.control_recording(&name, operation)
            }
            Action::EndRecording { name, mode } => self.end_recording(&name, mode),
            Action::PlayBridge { bridge, id, media } => self.play_bridge(bridge, id, media),
            Action::PlayChannel { channel, id, media } => self.play_channel(channel, id, media),
            Action::GetPlayback(id) => self.get_playback(&id),
            Action::ControlPlayback { id, operation } => {
                self.control_playback(&id, operation, timestamp)
            }
            Action::SendDtmf {
                channel,
                digits,
                timing,
            } => {
                if digits.is_empty() || digits.len() > 32 {
                    return Err(ControlError::Invalid("Invalid DTMF digits"));
                }
                let mut values = [DtmfDigit::Zero; 32];
                for (index, symbol) in digits.chars().enumerate() {
                    values[index] = DtmfDigit::from_symbol(symbol)
                        .ok_or(ControlError::Invalid("Invalid DTMF digits"))?;
                }
                let entry = self
                    .channels
                    .get_mut(&channel)
                    .ok_or(ControlError::NotFound("Channel not found"))?;
                if !matches!(entry.call.stasis(), StasisMembership::Entered(_)) {
                    return Err(ControlError::Conflict("Channel not in Stasis application"));
                }
                if entry.call.state() != CallState::Up {
                    return Err(ControlError::InvalidState("Channel in invalid state"));
                }
                let outbox = entry.dtmf.as_mut().ok_or(ControlError::Unsupported)?;
                let command = outbox.admit(&values[..digits.len()], timing)?;
                let mut outcome = Outcome::empty();
                outcome.effects.push(Effect::Dtmf {
                    key: CallKey {
                        id: channel,
                        generation: entry.call.generation(),
                    },
                    command,
                });
                Ok(outcome)
            }
            Action::ListChannels => Outcome::json(
                self.channels
                    .values()
                    .map(|entry| &entry.snapshot)
                    .collect::<Vec<_>>(),
            ),
            Action::GetChannel(id) => Outcome::json(self.channel(&id)?),
            Action::ListApplications => Outcome::json(
                self.applications
                    .keys()
                    .map(|name| self.application(name))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            Action::GetApplication(name) => Outcome::json(self.application(&name)?),
            Action::FilterApplication {
                application,
                update,
            } => self.filter_application(&application, update),
            Action::SubscribeResources {
                application,
                sources,
            } => self.change_subscriptions(&application, sources, subscriptions::Change::Subscribe),
            Action::UnsubscribeResources {
                application,
                sources,
            } => {
                self.change_subscriptions(&application, sources, subscriptions::Change::Unsubscribe)
            }
            Action::ListEndpoints { technology } => {
                Outcome::json(self.endpoint_list(technology.as_deref())?)
            }
            Action::GetEndpoint {
                technology,
                resource,
            } => Outcome::json(self.endpoint(&technology, &resource)?),
            Action::GetVariable { channel, variable } => {
                let entry = self
                    .channels
                    .get(&channel)
                    .ok_or(ControlError::NotFound("Channel not found"))?;
                if !matches!(entry.call.stasis(), StasisMembership::Entered(_)) {
                    return Err(ControlError::Conflict("Channel not in Stasis application"));
                }
                Outcome::json(rusterisk_api::Variable {
                    value: entry.variable(&variable)?,
                })
            }
            Action::SetVariable {
                channel,
                variable,
                value,
            } => self.set_variable(&channel, variable, value, timestamp, VariableAccess::Stasis),
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
                let application = self
                    .applications
                    .get(&app)
                    .copied()
                    .ok_or(ControlError::NotFound("Application not found"))?;
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
                let endpoint_resource = endpoint
                    .strip_prefix("PJSIP/")
                    .ok_or(ControlError::Invalid("Invalid endpoint technology"))?
                    .to_owned();
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
                    channelvars: None,
                };
                let mut outcome = Outcome::json(&snapshot)?;
                outcome.history.push(CallHistoryEvent::Started {
                    key: CallKey {
                        id: id.clone(),
                        generation,
                    },
                    timestamp: timestamp.into(),
                });
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
                    timeout: Some(timeout),
                });
                self.channels.insert(
                    id,
                    ChannelEntry {
                        call: Call::outgoing(generation).reserve_stasis(application)?,
                        endpoint: endpoint_resource,
                        snapshot,
                        arguments: args,
                        deadline: Some(deadline),
                        variables: BTreeMap::new(),
                        dtmf: None,
                        dialplan: Some(DialplanFrame::dormant(self.dialplan_program.clone())),
                        media_tasks: MediaTasks::new(generation),
                    },
                );
                Ok(outcome)
            }
            Action::Hangup(id) => self.end_at(&id, timestamp, 16, now_ms),
            Action::Continue {
                channel,
                context,
                extension,
                priority,
            } => self.continue_dialplan(&channel, context, extension, priority, timestamp),
            Action::Move { channel, app, args } => {
                self.move_application(&channel, app, args, timestamp)
            }
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
                if !matches!(entry.call.stasis(), StasisMembership::Entered(_)) {
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
                if !matches!(entry.call.stasis(), StasisMembership::Entered(_)) {
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
                    return Err(ControlError::BridgeExists(id));
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
                    video_mode: Some(rusterisk_api::BridgeVideoMode::None),
                    video_source_id: None,
                };
                let mut outcome = Outcome::json(&snapshot)?;
                for app in self.applications.keys() {
                    outcome.event(
                        app,
                        timestamp,
                        EventKind::BridgeCreated {
                            bridge: snapshot.clone(),
                        },
                    );
                }
                self.bridges.insert(
                    id,
                    BridgeEntry {
                        token,
                        snapshot,
                        media_tasks: MediaTasks::new(token),
                    },
                );
                Ok(outcome)
            }
            Action::DestroyBridge(id) => {
                let channels = self.bridge(&id)?.channels.clone();
                let mut outcome = self.membership(id.clone(), channels, false, timestamp)?;
                let mut bridge = self
                    .bridges
                    .remove(&id)
                    .ok_or(ControlError::NotFound("Bridge not found"))?;
                outcome
                    .ended_sources
                    .push(subscriptions::EndedSource::Bridge {
                        id: id.clone(),
                        generation: bridge.token,
                    });
                self.end_bridge_playbacks(&id, bridge.token, timestamp, &mut outcome);
                self.finish_owner_recordings(&MediaOwner::Bridge {
                    id: id.clone(),
                    generation: bridge.token,
                });
                let released = bridge.media_tasks.cancel();
                tracing::debug!(bridge = %id, generation = bridge.token, tasks = released.iter().flatten().count(), "bridge media tasks released");
                outcome.effects.push(Effect::ReleaseBridge {
                    generation: bridge.token,
                });
                for app in self.applications.keys() {
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
            if !matches!(entry.call.stasis(), StasisMembership::Entered(_)) {
                return Err(ControlError::Conflict("Channel not in Stasis application"));
            }
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
            outcome.optional_event(
                application_name(&self.applications, entry.call.stasis())?,
                timestamp,
                kind,
            );
        }
        let keys = snapshot
            .channels
            .iter()
            .map(|id| self.key(id))
            .collect::<Result<Vec<_>, _>>()?;
        outcome.effects.push(Effect::Bridge {
            id,
            generation: token,
            channels: keys,
        });
        Ok(outcome)
    }

    fn set_variable(
        &mut self,
        id: &ChannelId,
        variable: String,
        value: Option<String>,
        timestamp: &str,
        access: VariableAccess,
    ) -> Result<Outcome, ControlError> {
        let field = match variable.as_str() {
            "CALLERID(name)" => Some(CallerField::Name),
            "CALLERID(num)" => Some(CallerField::Number),
            "CALLERID(all)" => Some(CallerField::All),
            _ => None,
        };
        if let Some(field) = field {
            return self.set_caller(id, field, value.as_deref().unwrap_or(""), timestamp, access);
        }
        validate_variable(&variable)?;
        if matches!(
            variable.as_str(),
            "EXTEN" | "CONTEXT" | "PRIORITY" | "RUSTERISK_MEDIA_CONNECTED"
        ) {
            return Err(ControlError::Unsupported);
        }
        if value
            .as_ref()
            .is_some_and(|value| value.len() > 4096 || value.chars().any(char::is_control))
        {
            return Err(ControlError::Invalid("Invalid variable value"));
        }
        let entry = self
            .channels
            .get_mut(id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        access.check(entry.call.stasis())?;
        if let Some(value) = &value {
            let system_variables = ["STASISSTATUS", "DIALSTATUS", "PLAYBACKSTATUS"]
                .into_iter()
                .filter(|name| entry.variables.contains_key(*name))
                .count();
            let user_variables = entry.variables.len() - system_variables;
            if !matches!(
                variable.as_str(),
                "STASISSTATUS" | "DIALSTATUS" | "PLAYBACKSTATUS"
            ) && user_variables >= 64
                && !entry.variables.contains_key(&variable)
            {
                return Err(ControlError::Capacity);
            }
            entry.variables.insert(variable.clone(), value.clone());
        } else {
            entry.variables.remove(&variable);
        }
        let mut outcome = Outcome::empty();
        outcome.optional_event(
            application_name(&self.applications, entry.call.stasis())?,
            timestamp,
            EventKind::ChannelVarset {
                variable,
                value: value.unwrap_or_default(),
                channel: entry.snapshot.clone(),
            },
        );
        Ok(outcome)
    }

    fn set_caller(
        &mut self,
        id: &ChannelId,
        field: CallerField,
        value: &str,
        timestamp: &str,
        access: VariableAccess,
    ) -> Result<Outcome, ControlError> {
        if value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(ControlError::Invalid("Invalid caller identification"));
        }
        let entry = self
            .channels
            .get_mut(id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        access.check(entry.call.stasis())?;
        let mut caller = entry.snapshot.caller.clone();
        match field {
            CallerField::Name => caller.name = value.into(),
            CallerField::Number => caller.number = value.into(),
            CallerField::All => {
                caller = if let Some((name, number)) = value.split_once('<') {
                    let number = number
                        .trim()
                        .strip_suffix('>')
                        .ok_or(ControlError::Invalid("Invalid caller identification"))?;
                    Caller {
                        name: name.trim().trim_matches('"').into(),
                        number: number.trim().into(),
                    }
                } else {
                    Caller {
                        name: String::new(),
                        number: value.trim().into(),
                    }
                };
            }
        }
        if caller.name.len() > 128 || caller.number.len() > 128 {
            return Err(ControlError::Invalid(
                "Caller identification exceeds 128 bytes",
            ));
        }
        entry.snapshot.caller = caller;
        let mut outcome = Outcome::empty();
        outcome.optional_event(
            application_name(&self.applications, entry.call.stasis())?,
            timestamp,
            EventKind::ChannelCallerId {
                caller_presentation: 0,
                caller_presentation_txt: "Presentation Allowed, Not Screened".into(),
                channel: entry.snapshot.clone(),
            },
        );
        Ok(outcome)
    }

    fn end(
        &mut self,
        id: &ChannelId,
        timestamp: &str,
        cause: u16,
    ) -> Result<Outcome, ControlError> {
        self.end_at(id, timestamp, cause, 0)
    }

    fn end_at(
        &mut self,
        id: &ChannelId,
        timestamp: &str,
        cause: u16,
        now: u64,
    ) -> Result<Outcome, ControlError> {
        let branch = self.dial_branch(&self.key(id)?);
        let mut entry = self
            .channels
            .remove(id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        let application =
            application_name(&self.applications, entry.call.stasis())?.map(str::to_owned);
        let was_in_stasis = matches!(entry.call.stasis(), StasisMembership::Entered(_));
        let mut outcome = Outcome::empty();
        self.end_channel_playbacks(
            &CallKey {
                id: id.clone(),
                generation: entry.call.generation(),
            },
            application.as_deref(),
            timestamp,
            &mut outcome,
        );
        outcome
            .ended_sources
            .push(subscriptions::EndedSource::Channel {
                key: CallKey {
                    id: id.clone(),
                    generation: entry.call.generation(),
                },
                endpoint: entry.endpoint.clone(),
            });
        self.finish_owner_recordings(&MediaOwner::Channel(CallKey {
            id: id.clone(),
            generation: entry.call.generation(),
        }));
        if let Some(frame) = &mut entry.dialplan {
            if let Some(mut dial) = frame.dial.take() {
                outcome.append(self.cancel_dial(&mut dial, timestamp)?);
            }
        }
        if let Some(token) = entry.call.bridge() {
            for bridge in self
                .bridges
                .values_mut()
                .filter(|bridge| bridge.token == token)
            {
                bridge.snapshot.channels.retain(|value| value != id);
                outcome.optional_event(
                    application.as_deref(),
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
                    generation: bridge.token,
                    channels: keys,
                });
            }
        }
        entry.call = entry.call.apply(CallEvent::Hangup).call();
        outcome.history.push(CallHistoryEvent::Finished {
            key: CallKey {
                id: id.clone(),
                generation: entry.call.generation(),
            },
            timestamp: timestamp.into(),
        });
        let released = entry.media_tasks.cancel();
        tracing::debug!(channel = %id, generation = entry.call.generation(), tasks = released.iter().flatten().count(), "channel media tasks released");
        if let Some(outbox) = &mut entry.dtmf {
            outbox.cancel();
        }
        outcome.effects.push(Effect::Release {
            key: CallKey {
                id: id.clone(),
                generation: entry.call.generation(),
            },
        });
        if was_in_stasis {
            outcome.optional_event(
                application.as_deref(),
                timestamp,
                EventKind::StasisEnd {
                    channel: entry.snapshot.clone(),
                },
            );
        }
        outcome.optional_event(
            application.as_deref(),
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
        if let Some(branch) = branch {
            outcome.append(self.failed_dial_branch(branch, cause, now, timestamp)?);
        }
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
        if !matches!(entry.call.stasis(), StasisMembership::Entered(_)) {
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
            MediaOperation::Hold => outcome.optional_event(
                application_name(&self.applications, entry.call.stasis())?,
                timestamp,
                EventKind::ChannelHold {
                    channel: entry.snapshot.clone(),
                    musicclass: Some("N/A".into()),
                },
            ),
            MediaOperation::Unhold => outcome.optional_event(
                application_name(&self.applications, entry.call.stasis())?,
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
        self.signal_at(signal, timestamp, 0)
    }

    pub fn signal_at(
        &mut self,
        signal: Signal,
        timestamp: &str,
        now: u64,
    ) -> Result<Outcome, ControlError> {
        match signal {
            Signal::EndpointContact { key, presence } => {
                let mut outcome = Outcome::empty();
                if self.endpoints.observe(&key, presence) {
                    outcome.notice(
                        timestamp,
                        &EventKind::EndpointStateChange {
                            endpoint: self.endpoint("PJSIP", &key.resource)?,
                        },
                    );
                }
                Ok(outcome)
            }
            Signal::PeerDirection { key, direction } => {
                let mut outcome = Outcome::empty();
                if let Some(entry) = self.channels.get_mut(&key.id).filter(|entry| {
                    entry.call.generation() == key.generation
                        && entry.call.state() != CallState::Ended
                }) {
                    if entry.call.peer_direction() != direction {
                        let was_held = !entry.call.peer_direction().can_receive();
                        entry.call = entry.call.with_peer_direction(direction)?;
                        if entry.call.state() == CallState::Up {
                            outcome.effects.push(Effect::PeerDirection {
                                key,
                                direction,
                                gate: entry.call.audio_gate(),
                            });
                            let held = !direction.can_receive();
                            if held != was_held {
                                outcome.optional_event(
                                    application_name(&self.applications, entry.call.stasis())?,
                                    timestamp,
                                    if held {
                                        EventKind::ChannelHold {
                                            channel: entry.snapshot.clone(),
                                            musicclass: Some("default".into()),
                                        }
                                    } else {
                                        EventKind::ChannelUnhold {
                                            channel: entry.snapshot.clone(),
                                        }
                                    },
                                );
                            }
                        }
                    }
                }
                Ok(outcome)
            }
            Signal::RecordingAdmitted {
                owner,
                token,
                result,
            } => self.recording_admitted(&owner, token, result, timestamp),
            Signal::RecordingFinished {
                owner,
                token,
                result,
            } => self.finish_recording(&owner, token, result, timestamp),
            Signal::MediaPlaybackStarted { owner, token } => {
                self.playback_started(&owner, token, timestamp)
            }
            Signal::MediaPlaybackFinished {
                owner,
                token,
                completion,
            } => self.finish_playback(&owner, token, completion, timestamp),
            Signal::PlaybackStarted { key, token } => {
                self.playback_started(&MediaOwner::Channel(key), token, timestamp)
            }
            Signal::PlaybackFinished {
                key,
                token,
                completion,
            } => self.finish_playback(&MediaOwner::Channel(key), token, completion, timestamp),
            Signal::DtmfReady { key, events } => {
                if let Some(entry) = self
                    .channels
                    .get_mut(&key.id)
                    .filter(|entry| entry.call.generation() == key.generation)
                {
                    if entry.dtmf.is_none() {
                        entry.dtmf = Some(DtmfOutbox::new(events)?);
                    }
                }
                Ok(Outcome::empty())
            }
            Signal::DtmfFinished { key, token } => {
                if let Some(entry) = self
                    .channels
                    .get_mut(&key.id)
                    .filter(|entry| entry.call.generation() == key.generation)
                {
                    if let Some(outbox) = &mut entry.dtmf {
                        let _released = outbox.finish(token);
                    }
                }
                Ok(Outcome::empty())
            }
            Signal::DtmfReceived {
                key,
                digit,
                duration_ms,
            } => {
                let mut outcome = Outcome::empty();
                if let Some(entry) = self.channels.get(&key.id).filter(|entry| {
                    entry.call.generation() == key.generation
                        && entry.call.state() == CallState::Up
                        && matches!(entry.call.stasis(), StasisMembership::Entered(_))
                }) {
                    outcome.optional_event(
                        application_name(&self.applications, entry.call.stasis())?,
                        timestamp,
                        EventKind::ChannelDtmfReceived {
                            digit: digit.symbol().to_string(),
                            duration_ms,
                            channel: entry.snapshot.clone(),
                        },
                    );
                }
                Ok(outcome)
            }
            Signal::Terminated { key, cause } => {
                if self
                    .channels
                    .get(&key.id)
                    .is_some_and(|entry| entry.call.generation() == key.generation)
                {
                    self.end_at(&key.id, timestamp, cause, now)
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
                    return self.end_at(&key.id, timestamp, 16, now);
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
                            outcome.optional_event(
                                application_name(&self.applications, entry.call.stasis())?,
                                timestamp,
                                EventKind::ChannelStateChange {
                                    channel: entry.snapshot.clone(),
                                },
                            );
                            if let (CallState::Up, StasisMembership::Pending(application)) =
                                (entry.call.state(), entry.call.stasis())
                            {
                                entry.call = entry.call.enter_stasis(application)?;
                                entry.deadline = None;
                                entry.snapshot.dialplan.app_name = "Stasis".into();
                                entry.snapshot.dialplan.app_data =
                                    application_name(&self.applications, entry.call.stasis())?
                                        .ok_or(ControlError::NotFound("Application not found"))?
                                        .into();
                                outcome.optional_event(
                                    application_name(&self.applications, entry.call.stasis())?,
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
                            outcome.optional_event(
                                application_name(&self.applications, entry.call.stasis())?,
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
                if milestone == Milestone::Ringing {
                    outcome.append(self.ring_dial_branch(&key)?);
                }
                if milestone == Milestone::Answered {
                    if let Some(branch) = self.dial_branch(&key) {
                        outcome.append(self.answered_dial_branch(branch, now, timestamp)?);
                    }
                    outcome.append(self.connect_dial(&key)?);
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
            if self.channels.contains_key(&id) {
                outcome.append(self.end_at(&id, timestamp, 19, now_ms)?);
            }
        }
        Ok(outcome)
    }
}
