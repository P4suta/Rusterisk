use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
pub mod diagnosis;
mod event_filter;
mod event_source;
mod published_event;
pub use published_event::PublishedEvent;
mod external_rtp;
mod external_stream;
pub use external_stream::{AudioSocketTarget, InvalidAudioSocketSession, InvalidAudioSocketTarget};
mod media_commands;
mod media_event_parse;
mod media_events;
pub use media_event_parse::MediaEventError;
mod websocket_media_options;
pub use media_events::{MediaEvent, MediaFormat};
pub use websocket_media_options::WebSocketMediaOptions;
mod media_uri;
mod playback_options;
mod sound_id;
pub use media_commands::{MediaCommand, MediaCommandError, MediaControlFormat, MediaDirection};
pub use sound_id::{InvalidSoundId, SoundId};
mod recording_options;
mod recording_policy;
mod swagger;
pub use event_filter::EventFilterRequest;
pub use event_source::{EventSource, InvalidEventSource, ResourceEventSource};
pub use external_rtp::{ExternalRtpTarget, InvalidExternalRtpTarget, StaticRtpCodec};
pub use media_uri::{InvalidMediaUri, MediaUri};
pub use playback_options::{InvalidPlaybackOption, PlaybackMilliseconds, PlaybackOptions};
pub use recording_options::{
    InvalidRecordingOption, RecordingOptions, RecordingSeconds, RecordingTermination,
};
pub use recording_policy::{InvalidRecordingIfExists, RecordingIfExists};
pub use swagger::SwaggerError;

mod ari_route;
pub mod observation;
mod rtp_observation;
pub mod test_call;
pub use ari_route::{ari_route, is_known_operation, AriRoute};
pub use rtp_observation::{
    PeerReception, PeerReportSource, RtpObservation, ServerPacketCounts, ServerPacketSource,
};

pub const REFERENCE_VERSION: &str = "22.9.0";

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
pub struct ChannelId(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidId;

impl std::fmt::Display for InvalidId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("identifier must contain 1 through 128 ASCII letters, digits, dots, underscores, or hyphens")
    }
}

impl std::error::Error for InvalidId {}

pub fn is_valid_resource_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

impl TryFrom<String> for ChannelId {
    type Error = InvalidId;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if !is_valid_resource_id(&value) {
            return Err(InvalidId);
        }
        Ok(Self(value))
    }
}

impl From<ChannelId> for String {
    fn from(value: ChannelId) -> Self {
        value.0
    }
}

impl ChannelId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ChannelState {
    Down,
    Rsrved,
    OffHook,
    Dialing,
    Ring,
    Ringing,
    Up,
    Busy,
    #[serde(rename = "Dialing Offhook")]
    DialingOffhook,
    #[serde(rename = "Pre-ring")]
    PreRing,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Caller {
    pub name: String,
    pub number: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Dialplan {
    pub context: String,
    pub exten: String,
    pub priority: i32,
    #[serde(default)]
    pub app_name: String,
    #[serde(default)]
    pub app_data: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    pub state: ChannelState,
    pub accountcode: String,
    pub caller: Caller,
    pub connected: Caller,
    pub dialplan: Dialplan,
    pub creationtime: String,
    pub language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channelvars: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "Bridge")]
pub struct Bridge<Id = ChannelId> {
    pub id: Id,
    pub technology: String,
    pub bridge_type: String,
    pub bridge_class: String,
    pub creator: String,
    pub name: String,
    pub channels: Vec<ChannelId>,
    pub creationtime: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_mode: Option<BridgeVideoMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_source_id: Option<ChannelId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum BridgeVideoMode {
    None,
    Talker,
    Sfu,
    Single,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "Application")]
pub struct Application<Bridge = ChannelId> {
    pub name: String,
    pub channel_ids: Vec<ChannelId>,
    pub bridge_ids: Vec<Bridge>,
    pub endpoint_ids: Vec<String>,
    pub device_names: Vec<String>,
    pub events_allowed: Vec<EventFilter>,
    pub events_disallowed: Vec<EventFilter>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EventFilter {
    #[serde(rename = "type")]
    pub event_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Endpoint {
    pub technology: String,
    pub resource: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<EndpointState>,
    pub channel_ids: Vec<ChannelId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EndpointState {
    Unknown,
    Offline,
    Online,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "Playback")]
pub struct Playback<Id = ChannelId> {
    pub id: Id,
    pub media_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_media_uri: Option<String>,
    pub target_uri: String,
    pub language: String,
    pub state: PlaybackState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackState {
    Queued,
    Playing,
    Continuing,
    Paused,
    Stopped,
    Done,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "LiveRecording")]
pub struct LiveRecording<Id = String> {
    pub name: Id,
    pub format: String,
    pub state: RecordingState,
    pub target_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub talking_duration: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silence_duration: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RecordingState {
    Queued,
    Recording,
    Paused,
    Done,
    Failed,
    Canceled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "StoredRecording")]
pub struct StoredRecording<Id = String> {
    pub name: Id,
    pub format: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Sound {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub formats: Vec<SoundFormat>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SoundFormat {
    pub language: String,
    pub format: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApiError {
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "Event")]
pub struct Event<B = ChannelId, P = ChannelId, R = String> {
    pub timestamp: String,
    pub application: String,
    #[serde(flatten)]
    pub kind: EventKind<B, P, R>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
#[expect(
    clippy::large_enum_variant,
    reason = "docs/adr/keep-related-channel-snapshots-in-owned-events.md: Keep related channel snapshots inline."
)]
#[schemars(rename = "EventKind")]
pub enum EventKind<B = ChannelId, P = ChannelId, R = String> {
    EndpointStateChange {
        endpoint: Endpoint,
    },
    StasisStart {
        args: Vec<String>,
        channel: Channel,
    },
    StasisEnd {
        channel: Channel,
    },
    ChannelCreated {
        channel: Channel,
    },
    ChannelStateChange {
        channel: Channel,
    },
    ChannelDialplan {
        channel: Channel,
        dialplan_app: String,
        dialplan_app_data: String,
    },
    ChannelCallerId {
        caller_presentation: i32,
        caller_presentation_txt: String,
        channel: Channel,
    },
    ChannelDestroyed {
        cause: u16,
        cause_txt: String,
        channel: Channel,
    },
    ChannelHangupRequest {
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tech_cause: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        soft: Option<bool>,
        channel: Channel,
    },
    ChannelEnteredBridge {
        bridge: Bridge<B>,
        channel: Channel,
    },
    ChannelLeftBridge {
        bridge: Bridge<B>,
        channel: Channel,
    },
    BridgeCreated {
        bridge: Bridge<B>,
    },
    BridgeDestroyed {
        bridge: Bridge<B>,
    },
    ChannelDtmfReceived {
        digit: String,
        duration_ms: u32,
        channel: Channel,
    },
    ChannelHold {
        channel: Channel,
        #[serde(skip_serializing_if = "Option::is_none")]
        musicclass: Option<String>,
    },
    ChannelUnhold {
        channel: Channel,
    },
    ChannelVarset {
        variable: String,
        value: String,
        channel: Channel,
    },
    PlaybackStarted {
        playback: Playback<P>,
    },
    PlaybackContinuing {
        playback: Playback<P>,
    },
    PlaybackFinished {
        playback: Playback<P>,
    },
    RecordingStarted {
        recording: LiveRecording<R>,
    },
    RecordingFinished {
        recording: LiveRecording<R>,
    },
    RecordingFailed {
        recording: LiveRecording<R>,
    },
    ApplicationReplaced {},
    ApplicationMoveFailed {
        channel: Channel,
        destination: String,
        args: Vec<String>,
    },
    Dial {
        #[serde(skip_serializing_if = "Option::is_none")]
        caller: Option<Channel>,
        peer: Channel,
        #[serde(skip_serializing_if = "Option::is_none")]
        forward: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        forwarded: Option<Channel>,
        #[serde(skip_serializing_if = "Option::is_none")]
        dialstring: Option<String>,
        dialstatus: String,
    },
    ChannelUserevent {
        eventname: String,
        channel: Channel,
        userevent: std::collections::BTreeMap<String, String>,
    },
    #[serde(untagged)]
    #[schemars(skip, !untagged)]
    Published(PublishedEvent),
}

impl<B, P, R> EventKind<B, P, R> {
    pub const fn event_type(&self) -> &'static str {
        match self {
            Self::EndpointStateChange { .. } => "EndpointStateChange",
            Self::StasisStart { .. } => "StasisStart",
            Self::StasisEnd { .. } => "StasisEnd",
            Self::ChannelCreated { .. } => "ChannelCreated",
            Self::ChannelStateChange { .. } => "ChannelStateChange",
            Self::ChannelDialplan { .. } => "ChannelDialplan",
            Self::ChannelCallerId { .. } => "ChannelCallerId",
            Self::ChannelDestroyed { .. } => "ChannelDestroyed",
            Self::ChannelHangupRequest { .. } => "ChannelHangupRequest",
            Self::ChannelEnteredBridge { .. } => "ChannelEnteredBridge",
            Self::ChannelLeftBridge { .. } => "ChannelLeftBridge",
            Self::BridgeCreated { .. } => "BridgeCreated",
            Self::BridgeDestroyed { .. } => "BridgeDestroyed",
            Self::ChannelDtmfReceived { .. } => "ChannelDtmfReceived",
            Self::ChannelHold { .. } => "ChannelHold",
            Self::ChannelUnhold { .. } => "ChannelUnhold",
            Self::ChannelVarset { .. } => "ChannelVarset",
            Self::PlaybackStarted { .. } => "PlaybackStarted",
            Self::PlaybackContinuing { .. } => "PlaybackContinuing",
            Self::PlaybackFinished { .. } => "PlaybackFinished",
            Self::RecordingStarted { .. } => "RecordingStarted",
            Self::RecordingFinished { .. } => "RecordingFinished",
            Self::RecordingFailed { .. } => "RecordingFailed",
            Self::ApplicationReplaced { .. } => "ApplicationReplaced",
            Self::ApplicationMoveFailed { .. } => "ApplicationMoveFailed",
            Self::Dial { .. } => "Dial",
            Self::ChannelUserevent { .. } => "ChannelUserevent",
            Self::Published(event) => event.event_type(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

impl Method {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Post => "post",
            Self::Put => "put",
            Self::Delete => "delete",
        }
    }
    pub const fn upper(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Operation {
    pub id: &'static str,
    pub nickname: &'static str,
    pub method: Method,
    pub path: &'static str,
    pub response: &'static str,
    pub success_status: u16,
    pub parameters: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagementOperation {
    ReloadConfiguration,
    ChannelRtpObservation,
    DiagnoseExtension,
    DiagnoseChannel,
    DiagnoseRuntime,
    TestCall,
}

impl ManagementOperation {
    pub const fn metadata(self) -> &'static Operation {
        match self {
            Self::ReloadConfiguration => &Operation {
                id: "configuration.reload",
                nickname: "reloadConfiguration",
                method: Method::Post,
                path: "/rusterisk/configuration/reload",
                response: "void",
                success_status: 204,
                parameters: &[],
            },
            Self::ChannelRtpObservation => &Operation {
                id: "channels.rtpObservation",
                nickname: "rtpObservation",
                method: Method::Get,
                path: "/rusterisk/channels/{channelId}/rtp-statistics",
                response: "RtpObservation",
                success_status: 200,
                parameters: &[],
            },
            Self::DiagnoseExtension => &Operation {
                id: "diagnostics.extension",
                nickname: "diagnoseExtension",
                method: Method::Get,
                path: "/rusterisk/diagnostics/extensions/{context}/{extension}",
                response: "RouteDiagnosis",
                success_status: 200,
                parameters: &[],
            },
            Self::DiagnoseChannel => &Operation {
                id: "diagnostics.channel",
                nickname: "diagnoseChannel",
                method: Method::Get,
                path: "/rusterisk/diagnostics/channels/{channelId}",
                response: "ChannelDiagnosis",
                success_status: 200,
                parameters: &[],
            },
            Self::DiagnoseRuntime => &Operation {
                id: "diagnostics.runtime",
                nickname: "diagnoseRuntime",
                method: Method::Get,
                path: "/rusterisk/diagnostics/runtime",
                response: "RuntimeActivity",
                success_status: 200,
                parameters: &[],
            },
            Self::TestCall => &Operation {
                id: "testCalls.probe",
                nickname: "testCall",
                method: Method::Post,
                path: test_call::PATH,
                response: "TestCallResult",
                success_status: 200,
                parameters: &["deadline_seconds"],
            },
        }
    }
}

pub const MANAGEMENT_OPERATIONS: &[ManagementOperation] = &[
    ManagementOperation::ReloadConfiguration,
    ManagementOperation::ChannelRtpObservation,
    ManagementOperation::DiagnoseExtension,
    ManagementOperation::DiagnoseChannel,
    ManagementOperation::DiagnoseRuntime,
    ManagementOperation::TestCall,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterKind {
    String,
    Integer,
    Boolean,
    StringList,
    Variables,
    Filter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableParameterKind {
    String,
    Boolean,
}

impl From<UnavailableParameterKind> for ParameterKind {
    fn from(kind: UnavailableParameterKind) -> Self {
        match kind {
            UnavailableParameterKind::String => Self::String,
            UnavailableParameterKind::Boolean => Self::Boolean,
        }
    }
}

impl Operation {
    pub fn unavailable_parameters(&self) -> &'static [(&'static str, UnavailableParameterKind)] {
        use UnavailableParameterKind::{Boolean, String};
        match self.id {
            "channels.originate" | "channels.originateWithId" => &[
                ("label", String),
                ("otherChannelId", String),
                ("originator", String),
                ("formats", String),
            ],
            "channels.create" => &[
                ("otherChannelId", String),
                ("originator", String),
                ("formats", String),
            ],
            "channels.continue" => &[("label", String)],
            "bridges.addChannel" => &[
                ("absorbDTMF", Boolean),
                ("mute", Boolean),
                ("inhibitConnectedLineUpdates", Boolean),
                ("role", String),
            ],
            "bridges.play" | "bridges.playWithId" => &[("announcer_format", String)],
            "bridges.record" => &[("recorder_format", String)],
            _ => &[],
        }
    }

    pub fn parameter_kind(&self, name: &str) -> Option<ParameterKind> {
        if !self
            .parameters
            .iter()
            .any(|parameter| parameter.trim_end_matches('!') == name)
        {
            return None;
        }
        Some(match name {
            "deadline_seconds" | "timeout" | "priority" | "before" | "between" | "duration"
            | "after" | "maxDurationSeconds" | "maxSilenceSeconds" | "offsetms" | "skipms" => {
                ParameterKind::Integer
            }
            "beep" | "subscribeAll" => ParameterKind::Boolean,
            "channel" | "eventSource" | "media" | "only" => ParameterKind::StringList,
            "variables" => ParameterKind::Variables,
            "filter" => ParameterKind::Filter,
            _ => ParameterKind::String,
        })
    }

    pub fn matches(&self, method: &str, path: &str) -> bool {
        if self.method.upper() != method {
            return false;
        }
        let mut actual = path.split('/');
        for expected in self.path.split('/') {
            let Some(segment) = actual.next() else {
                return false;
            };
            if expected.starts_with('{') && expected.ends_with('}') {
                if segment.is_empty() {
                    return false;
                }
            } else if expected != segment {
                return false;
            }
        }
        actual.next().is_none()
    }

    pub fn resource(&self) -> &str {
        self.path
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or("")
    }
}

macro_rules! operation {
    ($id:literal, $name:literal, $method:ident, $path:literal, "void", [$($parameter:literal),*]) => {
        operation!($id, $name, $method, $path, "void", [$($parameter),*], 204)
    };
    ($id:literal, $name:literal, $method:ident, $path:literal, $response:literal, [$($parameter:literal),*]) => {
        operation!($id, $name, $method, $path, $response, [$($parameter),*], 200)
    };
    ($id:literal, $name:literal, $method:ident, $path:literal, $response:literal, [$($parameter:literal),*], $status:literal) => {
        Operation { id: $id, nickname: $name, method: Method::$method, path: $path, response: $response, parameters: &[$($parameter),*], success_status: $status }
    };
}

pub const OPERATIONS: &[Operation] = &[
    operation!("channels.list", "list", Get, "/channels", "Channel[]", []),
    operation!(
        "channels.originate",
        "originate",
        Post,
        "/channels",
        "Channel",
        [
            "endpoint!",
            "app",
            "appArgs",
            "channelId",
            "callerId",
            "timeout",
            "extension",
            "context",
            "priority",
            "variables"
        ]
    ),
    operation!(
        "channels.create",
        "create",
        Post,
        "/channels/create",
        "Channel",
        ["endpoint!", "app!", "appArgs", "channelId", "variables"]
    ),
    operation!(
        "channels.get",
        "get",
        Get,
        "/channels/{channelId}",
        "Channel",
        []
    ),
    operation!(
        "channels.originateWithId",
        "originateWithId",
        Post,
        "/channels/{channelId}",
        "Channel",
        [
            "endpoint!",
            "app",
            "appArgs",
            "callerId",
            "timeout",
            "extension",
            "context",
            "priority",
            "variables"
        ]
    ),
    operation!(
        "channels.hangup",
        "hangup",
        Delete,
        "/channels/{channelId}",
        "void",
        ["reason", "reason_code"]
    ),
    operation!(
        "channels.answer",
        "answer",
        Post,
        "/channels/{channelId}/answer",
        "void",
        []
    ),
    operation!(
        "channels.ring",
        "ring",
        Post,
        "/channels/{channelId}/ring",
        "void",
        []
    ),
    operation!(
        "channels.ringStop",
        "ringStop",
        Delete,
        "/channels/{channelId}/ring",
        "void",
        []
    ),
    operation!(
        "channels.dial",
        "dial",
        Post,
        "/channels/{channelId}/dial",
        "void",
        ["caller", "timeout"]
    ),
    operation!(
        "channels.continue",
        "continueInDialplan",
        Post,
        "/channels/{channelId}/continue",
        "void",
        ["context", "extension", "priority"]
    ),
    operation!(
        "channels.move",
        "move",
        Post,
        "/channels/{channelId}/move",
        "void",
        ["app!", "appArgs"]
    ),
    operation!(
        "channels.redirect",
        "redirect",
        Post,
        "/channels/{channelId}/redirect",
        "void",
        ["endpoint!"]
    ),
    operation!(
        "channels.hold",
        "hold",
        Post,
        "/channels/{channelId}/hold",
        "void",
        []
    ),
    operation!(
        "channels.unhold",
        "unhold",
        Delete,
        "/channels/{channelId}/hold",
        "void",
        []
    ),
    operation!(
        "channels.startMoh",
        "startMoh",
        Post,
        "/channels/{channelId}/moh",
        "void",
        ["mohClass"]
    ),
    operation!(
        "channels.stopMoh",
        "stopMoh",
        Delete,
        "/channels/{channelId}/moh",
        "void",
        []
    ),
    operation!(
        "channels.mute",
        "mute",
        Post,
        "/channels/{channelId}/mute",
        "void",
        ["direction"]
    ),
    operation!(
        "channels.unmute",
        "unmute",
        Delete,
        "/channels/{channelId}/mute",
        "void",
        ["direction"]
    ),
    operation!(
        "channels.sendDTMF",
        "sendDTMF",
        Post,
        "/channels/{channelId}/dtmf",
        "void",
        ["dtmf!", "before", "between", "duration", "after"]
    ),
    operation!(
        "channels.getChannelVar",
        "getChannelVar",
        Get,
        "/channels/{channelId}/variable",
        "Variable",
        ["variable!"]
    ),
    operation!(
        "channels.setChannelVar",
        "setChannelVar",
        Post,
        "/channels/{channelId}/variable",
        "void",
        ["variable!", "value"]
    ),
    operation!(
        "channels.play",
        "play",
        Post,
        "/channels/{channelId}/play",
        "Playback",
        ["media!", "lang", "offsetms", "skipms", "playbackId"],
        201
    ),
    operation!(
        "channels.playWithId",
        "playWithId",
        Post,
        "/channels/{channelId}/play/{playbackId}",
        "Playback",
        ["media!", "lang", "offsetms", "skipms"],
        201
    ),
    operation!(
        "channels.record",
        "record",
        Post,
        "/channels/{channelId}/record",
        "LiveRecording",
        [
            "name!",
            "format!",
            "maxDurationSeconds",
            "maxSilenceSeconds",
            "ifExists",
            "beep",
            "terminateOn"
        ],
        201
    ),
    operation!(
        "channels.externalMedia",
        "externalMedia",
        Post,
        "/channels/externalMedia",
        "Channel",
        [
            "app!",
            "external_host",
            "format!",
            "channelId",
            "encapsulation",
            "transport",
            "connection_type",
            "direction",
            "data",
            "transport_data",
            "variables"
        ]
    ),
    operation!("bridges.list", "list", Get, "/bridges", "Bridge[]", []),
    operation!(
        "bridges.create",
        "create",
        Post,
        "/bridges",
        "Bridge",
        ["type", "bridgeId", "name"]
    ),
    operation!(
        "bridges.get",
        "get",
        Get,
        "/bridges/{bridgeId}",
        "Bridge",
        []
    ),
    operation!(
        "bridges.createWithId",
        "createWithId",
        Post,
        "/bridges/{bridgeId}",
        "Bridge",
        ["type", "name"]
    ),
    operation!(
        "bridges.destroy",
        "destroy",
        Delete,
        "/bridges/{bridgeId}",
        "void",
        []
    ),
    operation!(
        "bridges.addChannel",
        "addChannel",
        Post,
        "/bridges/{bridgeId}/addChannel",
        "void",
        ["channel!"]
    ),
    operation!(
        "bridges.removeChannel",
        "removeChannel",
        Post,
        "/bridges/{bridgeId}/removeChannel",
        "void",
        ["channel!"]
    ),
    operation!(
        "bridges.play",
        "play",
        Post,
        "/bridges/{bridgeId}/play",
        "Playback",
        ["media!", "playbackId", "lang", "offsetms", "skipms"],
        201
    ),
    operation!(
        "bridges.playWithId",
        "playWithId",
        Post,
        "/bridges/{bridgeId}/play/{playbackId}",
        "Playback",
        ["media!", "lang", "offsetms", "skipms"],
        201
    ),
    operation!(
        "bridges.record",
        "record",
        Post,
        "/bridges/{bridgeId}/record",
        "LiveRecording",
        [
            "name!",
            "format!",
            "maxDurationSeconds",
            "maxSilenceSeconds",
            "ifExists",
            "beep",
            "terminateOn"
        ],
        201
    ),
    operation!(
        "applications.list",
        "list",
        Get,
        "/applications",
        "Application[]",
        []
    ),
    operation!(
        "applications.get",
        "get",
        Get,
        "/applications/{applicationName}",
        "Application",
        []
    ),
    operation!(
        "applications.subscribe",
        "subscribe",
        Post,
        "/applications/{applicationName}/subscription",
        "Application",
        ["eventSource!"]
    ),
    operation!(
        "applications.filter",
        "filter",
        Put,
        "/applications/{applicationName}/eventFilter",
        "Application",
        ["filter"]
    ),
    operation!(
        "applications.unsubscribe",
        "unsubscribe",
        Delete,
        "/applications/{applicationName}/subscription",
        "Application",
        ["eventSource!"]
    ),
    operation!(
        "events.eventWebsocket",
        "eventWebsocket",
        Get,
        "/events",
        "Event",
        ["app!", "subscribeAll"]
    ),
    operation!(
        "endpoints.list",
        "list",
        Get,
        "/endpoints",
        "Endpoint[]",
        []
    ),
    operation!(
        "endpoints.listByTech",
        "listByTech",
        Get,
        "/endpoints/{tech}",
        "Endpoint[]",
        []
    ),
    operation!(
        "endpoints.get",
        "get",
        Get,
        "/endpoints/{tech}/{resource}",
        "Endpoint",
        []
    ),
    operation!(
        "playbacks.get",
        "get",
        Get,
        "/playbacks/{playbackId}",
        "Playback",
        []
    ),
    operation!(
        "playbacks.stop",
        "stop",
        Delete,
        "/playbacks/{playbackId}",
        "void",
        []
    ),
    operation!(
        "playbacks.control",
        "control",
        Post,
        "/playbacks/{playbackId}/control",
        "void",
        ["operation!"]
    ),
    operation!(
        "recordings.listStored",
        "listStored",
        Get,
        "/recordings/stored",
        "StoredRecording[]",
        []
    ),
    operation!(
        "recordings.getStored",
        "getStored",
        Get,
        "/recordings/stored/{recordingName}",
        "StoredRecording",
        []
    ),
    operation!(
        "recordings.deleteStored",
        "deleteStored",
        Delete,
        "/recordings/stored/{recordingName}",
        "void",
        []
    ),
    operation!(
        "recordings.getStoredFile",
        "getStoredFile",
        Get,
        "/recordings/stored/{recordingName}/file",
        "binary",
        []
    ),
    operation!(
        "recordings.copyStored",
        "copyStored",
        Post,
        "/recordings/stored/{recordingName}/copy",
        "StoredRecording",
        ["destinationRecordingName!"]
    ),
    operation!(
        "recordings.getLive",
        "getLive",
        Get,
        "/recordings/live/{recordingName}",
        "LiveRecording",
        []
    ),
    operation!(
        "recordings.cancel",
        "cancel",
        Delete,
        "/recordings/live/{recordingName}",
        "void",
        []
    ),
    operation!(
        "recordings.stop",
        "stop",
        Post,
        "/recordings/live/{recordingName}/stop",
        "void",
        []
    ),
    operation!(
        "recordings.pause",
        "pause",
        Post,
        "/recordings/live/{recordingName}/pause",
        "void",
        []
    ),
    operation!(
        "recordings.unpause",
        "unpause",
        Delete,
        "/recordings/live/{recordingName}/pause",
        "void",
        []
    ),
    operation!(
        "recordings.mute",
        "mute",
        Post,
        "/recordings/live/{recordingName}/mute",
        "void",
        []
    ),
    operation!(
        "recordings.unmute",
        "unmute",
        Delete,
        "/recordings/live/{recordingName}/mute",
        "void",
        []
    ),
    operation!(
        "sounds.list",
        "list",
        Get,
        "/sounds",
        "Sound[]",
        ["lang", "format"]
    ),
    operation!("sounds.get", "get", Get, "/sounds/{soundId}", "Sound", []),
    operation!(
        "asterisk.getInfo",
        "getInfo",
        Get,
        "/asterisk/info",
        "AsteriskInfo",
        ["only"]
    ),
    operation!(
        "asterisk.ping",
        "ping",
        Get,
        "/asterisk/ping",
        "AsteriskPing",
        []
    ),
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Variable {
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AsteriskPing {
    pub ping: String,
    pub timestamp: String,
    pub asterisk_id: String,
}

mod info_section;
pub use info_section::InfoSection;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AsteriskInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BuildInfo {
    pub os: String,
    pub kernel: String,
    pub options: String,
    pub machine: String,
    pub date: String,
    pub user: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SetId {
    pub user: String,
    pub group: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigInfo {
    pub name: String,
    pub default_language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_channels: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_open_files: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_load: Option<f64>,
    pub setid: SetId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SystemInfo {
    pub version: String,
    pub entity_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusInfo {
    pub startup_time: String,
    pub last_reload_time: String,
}

pub fn schemas() -> Value {
    let mut settings = schemars::generate::SchemaSettings::draft2020_12();
    settings.definitions_path = "/components/schemas".into();
    let mut generator = settings.into_generator();
    let _channel = generator.subschema_for::<Channel>();
    let _observation = generator.subschema_for::<observation::WatchRecord>();
    let _route_diagnosis = generator.subschema_for::<diagnosis::RouteDiagnosis>();
    let _runtime_activity = generator.subschema_for::<diagnosis::RuntimeActivity>();
    let _channel_diagnosis = generator.subschema_for::<diagnosis::ChannelDiagnosis>();
    let _rtp = generator.subschema_for::<RtpObservation>();
    let _test_call = generator.subschema_for::<test_call::TestCallResult>();
    let _bridge = generator.subschema_for::<Bridge>();
    let _application = generator.subschema_for::<Application>();
    let _filter = generator.subschema_for::<EventFilterRequest>();
    let _endpoint = generator.subschema_for::<Endpoint>();
    let _playback = generator.subschema_for::<Playback>();
    let _live = generator.subschema_for::<LiveRecording>();
    let _stored = generator.subschema_for::<StoredRecording>();
    let _sound = generator.subschema_for::<Sound>();
    let _event = generator.subschema_for::<Event>();
    let _error = generator.subschema_for::<ApiError>();
    let _variable = generator.subschema_for::<Variable>();
    let _info = generator.subschema_for::<AsteriskInfo>();
    let _ping = generator.subschema_for::<AsteriskPing>();
    Value::Object(generator.take_definitions(true))
}

fn response_schema(name: &str) -> Value {
    if let Some(item) = name.strip_suffix("[]") {
        json!({"type": "array", "items": {"$ref": format!("#/components/schemas/{item}")}})
    } else {
        json!({"$ref": format!("#/components/schemas/{name}")})
    }
}

fn path_parameters(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter_map(|segment| {
        segment
            .strip_prefix('{')
            .and_then(|value| value.strip_suffix('}'))
    })
}

fn parameter_schema(operation: &Operation, name: &str, kind: ParameterKind) -> Value {
    match name {
        "deadline_seconds" => json!({
            "type": "integer", "minimum": 1, "maximum": test_call::MAX_SECONDS,
            "default": test_call::DEFAULT_SECONDS,
        }),
        "only" => {
            json!({"type": "array", "items": schemars::schema_for!(InfoSection).to_value(), "maxItems": 32})
        }
        "ifExists" => schemars::schema_for!(RecordingIfExists).to_value(),
        "priority"
            if matches!(
                operation.id,
                "channels.originate" | "channels.originateWithId"
            ) =>
        {
            json!({"type": "integer", "format": "int64"})
        }
        "offsetms" | "skipms" => json!({
            "type": "integer", "format": "int32", "minimum": 0, "maximum": i32::MAX,
            "default": if name == "offsetms" { 0 } else { 3000 },
        }),
        "maxDurationSeconds" | "maxSilenceSeconds" => {
            schemars::schema_for!(RecordingSeconds).to_value()
        }
        "terminateOn" => schemars::schema_for!(RecordingTermination).to_value(),
        _ => match kind {
            ParameterKind::String => json!({"type": "string"}),
            ParameterKind::Integer => json!({"type": "integer", "format": "int32"}),
            ParameterKind::Boolean => json!({"type": "boolean"}),
            ParameterKind::StringList => {
                json!({"type": "array", "items": {"type": "string"}, "maxItems": 32})
            }
            ParameterKind::Variables => {
                json!({"type": "object", "additionalProperties": {"type": "string"}, "maxProperties": 64})
            }
            ParameterKind::Filter => json!({"type": "object"}),
        },
    }
}

fn parameters(operation: &Operation, format: Option<DiscoveryFormat>) -> Vec<Value> {
    let paths = path_parameters(operation.path).map(|name| (name, true, "path"));
    let queries = operation
        .parameters
        .iter()
        .map(|name| (name.trim_end_matches('!'), name.ends_with('!'), "query"));
    paths.chain(queries).filter(|(name, _, _)| format.is_some() || !matches!(*name, "variables" | "filter")).map(|(name, required, location)| {
        let location = if matches!(name, "variables" | "filter") { "body" } else { location };
        let parameter_kind = operation.parameter_kind(name).unwrap_or(ParameterKind::String);
        let multiple = parameter_kind == ParameterKind::StringList;
        let schema = parameter_schema(operation, name, parameter_kind);
        if let Some(format) = format {
            let required = required && !(format == DiscoveryFormat::Asterisk
                && location == "path" && operation.resource() == "endpoints");
            let kind = if multiple && format == DiscoveryFormat::Asterisk { "string" } else { schema["type"].as_str().unwrap_or("string") };
            let data_type = match (name, kind, format) {
                ("variables", _, _) => "containers",
                (_, "integer", DiscoveryFormat::Asterisk) if schema["format"] == "int64" => "long",
                (_, "integer", DiscoveryFormat::Asterisk) => "int",
                _ => kind,
            };
            let mut parameter = json!({"name": name, "paramType": location, "required": required, "allowMultiple": multiple, "dataType": data_type, "type": kind, "description": name});
            for field in ["format", "minimum", "maximum", "items", "maxItems"] {
                if let Some(value) = schema.get(field) { parameter[field] = value.clone(); }
            }
            if let Some(value) = schema.get("default") { parameter["defaultValue"] = value.clone(); }
            if let Some(values) = schema.get("enum").or_else(|| {
                (multiple && format == DiscoveryFormat::Asterisk)
                    .then(|| schema.get("items").and_then(|items| items.get("enum")))
                    .flatten()
            }) {
                match format {
                    DiscoveryFormat::Asterisk => parameter["allowableValues"] = json!({"valueType": "LIST", "values": values}),
                    DiscoveryFormat::Standard => parameter["enum"] = values.clone(),
                }
            }
            if multiple && format == DiscoveryFormat::Standard { parameter["collectionFormat"] = "csv".into(); }
            parameter
        } else {
            let mut parameter = json!({"name": name, "in": location, "required": required, "schema": schema});
            if multiple { parameter["style"] = "form".into(); parameter["explode"] = false.into(); }
            parameter
        }
    }).collect()
}

pub fn openapi() -> Value {
    let mut paths = Map::new();
    for operation in OPERATIONS {
        let response = if operation.response == "void" {
            json!({"description": "Completed"})
        } else if operation.response == "binary" {
            json!({"description": "Stored audio bytes", "content": {"audio/wav": {}}})
        } else {
            json!({"description": "Accepted operation", "content": {"application/json": {"schema": response_schema(operation.response)}}})
        };
        let mut responses = Map::new();
        responses.insert(operation.success_status.to_string(), response);
        let entry = paths.entry(operation.path).or_insert_with(|| json!({}));
        let security = json_array([json!({"basicAuth": []})]);
        entry[operation.method.as_str()] = json!({"operationId": operation.id, "parameters": parameters(operation, None), "responses": responses, "security": security});
        if !operation.parameters.is_empty()
            && matches!(
                operation.method,
                Method::Post | Method::Put | Method::Delete
            )
        {
            let properties: Map<String, Value> = operation
                .parameters
                .iter()
                .filter_map(|name| {
                    let name = name.trim_end_matches('!');
                    operation
                        .parameter_kind(name)
                        .map(|kind| (name.to_owned(), parameter_schema(operation, name, kind)))
                })
                .collect();
            entry[operation.method.as_str()]["requestBody"] = json!({"content": {"application/json": {"schema": {"type": "object", "properties": properties}}}});
        }
        if operation.parameters.contains(&"filter") {
            entry[operation.method.as_str()]["requestBody"] = json!({"content": {"application/json": {"schema": {"$ref": "#/components/schemas/EventFilterRequest"}}}});
        }
    }
    for operation in MANAGEMENT_OPERATIONS {
        let operation = operation.metadata();
        let mut responses = Map::new();
        responses.insert(
            operation.success_status.to_string(),
            if operation.response == "void" {
                json!({"description": "Completed"})
            } else {
                json!({"description": "Observation", "content":{"application/json":{"schema": response_schema(operation.response)}}})
            },
        );
        let entry = paths.entry(operation.path).or_insert_with(|| json!({}));
        entry["servers"] = json_array([json!({"url": "/"})]);
        let security = json_array([json!({"basicAuth": []})]);
        entry[operation.method.as_str()] = json!({"operationId": operation.id, "parameters": parameters(operation, None), "responses": responses, "security": security});
    }
    let servers = json_array([json!({"url": "/ari"})]);
    observation::describe(&mut paths);
    json!({"openapi": "3.1.0", "info": {"title": "Rusterisk", "version": env!("CARGO_PKG_VERSION")}, "servers": servers, "paths": paths, "components": {"schemas": schemas(), "securitySchemes": {"basicAuth": {"type": "http", "scheme": "basic"}}}})
}

fn json_array<const N: usize>(values: [Value; N]) -> Value {
    Value::Array(values.into_iter().collect())
}

pub fn swagger(resource: &str) -> Result<Value, SwaggerError> {
    swagger_for(resource, DiscoveryFormat::Standard)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoveryFormat {
    Asterisk,
    Standard,
}

impl DiscoveryFormat {
    const fn version(self) -> &'static str {
        match self {
            Self::Asterisk => "1.1",
            Self::Standard => "1.2",
        }
    }
}

pub fn swagger_for(resource: &str, format: DiscoveryFormat) -> Result<Value, SwaggerError> {
    let mut paths: std::collections::BTreeMap<&str, Vec<Value>> = std::collections::BTreeMap::new();
    for operation in OPERATIONS
        .iter()
        .filter(|operation| operation.resource() == resource)
    {
        let response = if let Some(item) = operation.response.strip_suffix("[]") {
            format!("List[{item}]")
        } else {
            operation.response.into()
        };
        let message_field = match format {
            DiscoveryFormat::Asterisk => "reason",
            DiscoveryFormat::Standard => "message",
        };
        let errors: Vec<_> = [
            (400, "Invalid parameters"),
            (401, "Authentication required"),
            (403, "Operation forbidden"),
            (404, "Resource not found"),
            (409, "Invalid resource state"),
            (412, "Channel in invalid state"),
            (422, "Event source does not exist"),
            (500, "Operation failed"),
            (501, "Operation not supported"),
            (503, "Resource capacity exceeded"),
        ]
        .into_iter()
        .map(|(code, message)| {
            let mut error = json!({"code": code});
            error[message_field] = Value::String(message.into());
            error
        })
        .collect();
        let mut definition = json!({"nickname": operation.nickname, "summary": operation.nickname, "parameters": parameters(operation, Some(format))});
        match format {
            DiscoveryFormat::Asterisk => {
                definition["httpMethod"] = Value::String(operation.method.upper().into());
                definition["responseClass"] = Value::String(response);
                definition["errorResponses"] = Value::Array(errors);
            }
            DiscoveryFormat::Standard => {
                definition["method"] = Value::String(operation.method.upper().into());
                definition["responseMessages"] = Value::Array(errors);
                if let Some(item) = operation.response.strip_suffix("[]") {
                    definition["type"] = Value::String("array".into());
                    definition["items"] = json!({"$ref": item});
                } else if operation.response == "binary" {
                    definition["type"] = Value::String("string".into());
                    definition["format"] = Value::String("byte".into());
                    definition["produces"] = json_array([Value::String("audio/wav".into())]);
                } else {
                    definition["type"] = Value::String(operation.response.into());
                }
            }
        }
        paths.entry(operation.path).or_default().push(definition);
    }
    let apis: Vec<Value> = paths
        .into_iter()
        .map(|(path, operations)| json!({"path": path, "operations": operations}))
        .collect();
    let resource_path = match format {
        DiscoveryFormat::Asterisk => format!("/api-docs/{resource}.{{format}}"),
        DiscoveryFormat::Standard => format!("/{resource}"),
    };
    Ok(
        json!({"swaggerVersion": format.version(), "apiVersion": "2.0.0", "basePath": "/ari", "resourcePath": resource_path, "apis": apis, "models": swagger::models()?}),
    )
}

pub fn resources() -> Value {
    resources_for(DiscoveryFormat::Standard)
}

pub fn resources_for(format: DiscoveryFormat) -> Value {
    let names: std::collections::BTreeSet<&str> =
        OPERATIONS.iter().map(Operation::resource).collect();
    let apis: Vec<Value> = names.into_iter().map(|resource| json!({"path": format!("/api-docs/{resource}.{{format}}"), "description": resource})).collect();
    let base = match format {
        DiscoveryFormat::Asterisk => "/ari",
        DiscoveryFormat::Standard => "/rusterisk",
    };
    json!({"swaggerVersion": format.version(), "apiVersion": "2.0.0", "basePath": base, "apis": apis})
}
