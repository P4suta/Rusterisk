use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
mod media_uri;
mod recording_options;
mod recording_policy;
mod swagger;
pub use media_uri::{InvalidMediaUri, MediaUri};
pub use recording_options::{
    InvalidRecordingOption, RecordingOptions, RecordingSeconds, RecordingTermination,
};
pub use recording_policy::{InvalidRecordingIfExists, RecordingIfExists};
pub use swagger::SwaggerError;

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

impl TryFrom<String> for ChannelId {
    type Error = InvalidId;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || value.len() > 128
            || value == "."
            || value == ".."
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Bridge {
    pub id: ChannelId,
    pub technology: String,
    pub bridge_type: String,
    pub bridge_class: String,
    pub creator: String,
    pub name: String,
    pub channels: Vec<ChannelId>,
    pub creationtime: String,
    pub video_mode: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Application {
    pub name: String,
    pub channel_ids: Vec<ChannelId>,
    pub bridge_ids: Vec<ChannelId>,
    pub endpoint_ids: Vec<String>,
    pub device_names: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Endpoint {
    pub technology: String,
    pub resource: String,
    pub state: String,
    pub channel_ids: Vec<ChannelId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Playback {
    pub id: ChannelId,
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
pub struct LiveRecording {
    pub name: String,
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
pub struct StoredRecording {
    pub name: String,
    pub format: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Sound {
    pub id: String,
    pub text: String,
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
pub struct Event {
    pub timestamp: String,
    pub application: String,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum EventKind {
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
        cause: u16,
        soft: bool,
        channel: Channel,
    },
    ChannelEnteredBridge {
        bridge: Bridge,
        channel: Channel,
    },
    ChannelLeftBridge {
        bridge: Bridge,
        channel: Channel,
    },
    BridgeCreated {
        bridge: Bridge,
    },
    BridgeDestroyed {
        bridge: Bridge,
    },
    ChannelDtmfReceived {
        digit: String,
        duration_ms: u32,
        channel: Channel,
    },
    ChannelHold {
        channel: Channel,
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
        playback: Playback,
    },
    PlaybackContinuing {
        playback: Playback,
    },
    PlaybackFinished {
        playback: Playback,
    },
    RecordingStarted {
        recording: LiveRecording,
    },
    RecordingFinished {
        recording: LiveRecording,
    },
    RecordingFailed {
        recording: LiveRecording,
    },
    ApplicationReplaced {},
    ApplicationMoveFailed {
        channel: Channel,
        destination: String,
        args: Vec<String>,
    },
    Dial {
        caller: Channel,
        peer: Channel,
        dialstatus: String,
    },
    ChannelUserevent {
        eventname: String,
        channel: Channel,
        userevent: std::collections::BTreeMap<String, String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Delete,
}

impl Method {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Post => "post",
            Self::Delete => "delete",
        }
    }
    pub const fn upper(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
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

impl Operation {
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

const UNSUPPORTED_OPERATIONS: &[Operation] = &[
    operation!(
        "channels.snoopChannel",
        "snoopChannel",
        Post,
        "/channels/{channelId}/snoop",
        "Channel",
        ["app!", "spy", "whisper", "appArgs", "snoopId"]
    ),
    operation!(
        "channels.snoopChannelWithId",
        "snoopChannelWithId",
        Post,
        "/channels/{channelId}/snoop/{snoopId}",
        "Channel",
        ["app!", "spy", "whisper", "appArgs"]
    ),
];

pub fn is_known_operation(method: &str, path: &str) -> bool {
    OPERATIONS
        .iter()
        .chain(UNSUPPORTED_OPERATIONS)
        .any(|operation| operation.matches(method, path))
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
        ["media!", "lang", "playbackId"],
        201
    ),
    operation!(
        "channels.playWithId",
        "playWithId",
        Post,
        "/channels/{channelId}/play/{playbackId}",
        "Playback",
        ["media!", "lang"],
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
            "external_host!",
            "format!",
            "channelId",
            "encapsulation",
            "transport",
            "connection_type",
            "direction",
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
        ["channel!", "role"]
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
        ["media!", "playbackId", "lang"],
        201
    ),
    operation!(
        "bridges.playWithId",
        "playWithId",
        Post,
        "/bridges/{bridgeId}/play/{playbackId}",
        "Playback",
        ["media!", "lang"],
        201
    ),
    operation!(
        "bridges.record",
        "record",
        Post,
        "/bridges/{bridgeId}/record",
        "LiveRecording",
        ["name!", "format!", "maxDurationSeconds", "ifExists"],
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AsteriskInfo {
    pub system: SystemInfo,
    pub status: StatusInfo,
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
    let _bridge = generator.subschema_for::<Bridge>();
    let _application = generator.subschema_for::<Application>();
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

fn parameters(operation: &Operation, format: Option<DiscoveryFormat>) -> Vec<Value> {
    let paths = path_parameters(operation.path).map(|name| (name, true, "path"));
    let queries = operation
        .parameters
        .iter()
        .map(|name| (name.trim_end_matches('!'), name.ends_with('!'), "query"));
    paths.chain(queries).filter(|(name, _, _)| format.is_some() || *name != "variables").map(|(name, required, location)| {
        let location = if name == "variables" { "body" } else { location };
        let schema = match name {
            "ifExists" => schemars::schema_for!(RecordingIfExists).to_value(),
            "maxDurationSeconds" | "maxSilenceSeconds" => schemars::schema_for!(RecordingSeconds).to_value(),
            "terminateOn" => schemars::schema_for!(RecordingTermination).to_value(),
            "beep" => schemars::schema_for!(bool).to_value(),
            _ => json!({"type": "string"}),
        };
        if let Some(format) = format {
            let kind = schema["type"].as_str().unwrap_or("string");
            let data_type = if name == "variables" { "containers" } else if kind == "integer" && format == DiscoveryFormat::Asterisk { "int" } else { kind };
            let mut parameter = json!({"name": name, "paramType": location, "required": required, "allowMultiple": false, "dataType": data_type, "type": kind, "description": name});
            for field in ["format", "minimum", "maximum"] {
                if let Some(value) = schema.get(field) { parameter[field] = value.clone(); }
            }
            if let Some(values) = schema.get("enum") {
                match format {
                    DiscoveryFormat::Asterisk => parameter["allowableValues"] = json!({"valueType": "LIST", "values": values}),
                    DiscoveryFormat::Standard => parameter["enum"] = values.clone(),
                }
            }
            parameter
        } else { json!({"name": name, "in": location, "required": required, "schema": schema}) }
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
        if operation.parameters.contains(&"variables") {
            entry[operation.method.as_str()]["requestBody"] = json!({"content": {"application/json": {"schema": {"type": "object", "properties": {"variables": {"type": "object", "additionalProperties": {"type": "string"}}}}}}});
        }
    }
    let servers = json_array([json!({"url": "/ari"})]);
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
