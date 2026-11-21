use crate::media::{PlaybackControl, RecordingEnd};
use crate::{Action, ControlError, Controller, Credentials, Outcome};
use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{any, get},
    Json, Router,
};
use base64::Engine;
use rusterisk_api::{ChannelId, InvalidMediaUri, MediaUri};
use rusterisk_core::{DtmfTiming, MuteDirection, RecordingControl};
use serde_json::{json, Value};
use std::collections::BTreeMap;
mod configuration;
mod diagnosis;
mod external_media;
mod observation;
mod recording;
mod request_body;
mod rtp_observation;

pub fn router(controller: Controller) -> Router {
    with_management(
        router_for_scheme(controller.clone(), HttpScheme::Http),
        controller,
        None,
    )
}

pub fn router_with_media(controller: Controller, media: crate::media::MediaPort) -> Router {
    with_management(
        router_with_media_for_scheme(controller.clone(), media.clone(), HttpScheme::Http),
        controller,
        Some(media),
    )
}

pub(crate) fn with_management(
    application: Router,
    controller: Controller,
    media: Option<crate::media::MediaPort>,
) -> Router {
    application
        .merge(configuration::router(controller.clone()))
        .merge(observation::router(controller.clone()))
        .merge(diagnosis::router(controller.clone()))
        .merge(rtp_observation::router(controller, media))
}

pub(crate) fn router_with_media_for_scheme(
    controller: Controller,
    media: crate::media::MediaPort,
    scheme: HttpScheme,
) -> Router {
    let state = controller.clone();
    router_for_scheme(controller, scheme).route(
        "/media/{connection_id}",
        get(
            move |uri: Uri,
                  headers: HeaderMap,
                  upgrade: Result<
                axum::extract::ws::WebSocketUpgrade,
                axum::extract::ws::rejection::WebSocketUpgradeRejection,
            >| {
                crate::media_websocket::upgrade(state.clone(), media.clone(), uri, headers, upgrade)
            },
        ),
    )
}

#[derive(Clone, Copy)]
pub(crate) enum HttpScheme {
    Http,
    Https,
}

pub(crate) fn router_for_scheme(controller: Controller, scheme: HttpScheme) -> Router {
    let handler =
        move |State(controller): State<Controller>,
              method: Method,
              uri: Uri,
              headers: HeaderMap,
              body: Bytes| { api(controller, scheme, method, uri, headers, body) };
    Router::new()
        .route(
            "/ari/events",
            get(crate::websocket::upgrade)
                .head(handler)
                .fallback(handler),
        )
        .fallback(any(handler))
        .layer(DefaultBodyLimit::max(16384))
        .with_state(controller)
}

pub(crate) fn error(error: ControlError) -> Response {
    let status = match error.status() {
        400 => StatusCode::BAD_REQUEST,
        401 => StatusCode::UNAUTHORIZED,
        403 => StatusCode::FORBIDDEN,
        404 => StatusCode::NOT_FOUND,
        409 => StatusCode::CONFLICT,
        412 => StatusCode::PRECONDITION_FAILED,
        422 => StatusCode::UNPROCESSABLE_ENTITY,
        501 => StatusCode::NOT_IMPLEMENTED,
        503 => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let mut response = (
        status,
        Json(rusterisk_api::ApiError {
            message: error.to_string(),
        }),
    )
        .into_response();
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            "WWW-Authenticate",
            axum::http::HeaderValue::from_static("Basic realm=\"Rusterisk\""),
        );
    }
    response
}

pub(crate) fn credentials(
    headers: &HeaderMap,
    parameters: &BTreeMap<String, String>,
) -> Result<Credentials, ControlError> {
    let text = if let Some(header) = headers.get("Authorization") {
        let value = header.to_str().map_err(|_| ControlError::Unauthorized)?;
        if value.len() > 1024 {
            return Err(ControlError::Unauthorized);
        }
        let encoded = value
            .strip_prefix("Basic ")
            .ok_or(ControlError::Unauthorized)?;
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| ControlError::Unauthorized)?,
        )
        .map_err(|_| ControlError::Unauthorized)?
    } else {
        parameters
            .get("api_key")
            .cloned()
            .ok_or(ControlError::Unauthorized)?
    };
    let (username, password) = text.split_once(':').ok_or(ControlError::Unauthorized)?;
    Credentials::new(username.into(), password.into())
}

pub(crate) fn parameters(uri: &Uri) -> Result<BTreeMap<String, String>, ControlError> {
    if uri.to_string().len() > 4096 {
        return Err(ControlError::Invalid("Request URI too long"));
    }
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(uri.query().unwrap_or(""))
        .map_err(|_| ControlError::Invalid("Invalid query parameters"))?;
    if pairs.len() > 32 {
        return Err(ControlError::Invalid("Too many query parameters"));
    }
    let mut parameters = BTreeMap::new();
    for (key, value) in pairs {
        if parameters.insert(key, value).is_some() {
            return Err(ControlError::Invalid("Duplicate query parameter"));
        }
    }
    Ok(parameters)
}

fn required<'a>(
    parameters: &'a BTreeMap<String, String>,
    key: &str,
) -> Result<&'a str, ControlError> {
    parameters
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(ControlError::Invalid("Missing required parameter"))
}
fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}
fn optional_id(
    parameters: &BTreeMap<String, String>,
    key: &str,
) -> Result<ChannelId, ControlError> {
    id(parameters
        .get(key)
        .map(String::as_str)
        .unwrap_or(&uuid::Uuid::new_v4().to_string()))
}

fn originate(
    parameters: &BTreeMap<String, String>,
    channel: ChannelId,
) -> Result<Action, ControlError> {
    Ok(Action::Originate {
        id: channel,
        endpoint: required(parameters, "endpoint")?.into(),
        app: required(parameters, "app")?.into(),
        args: parameters
            .get("appArgs")
            .map(|value| value.split(',').map(String::from).collect())
            .unwrap_or_default(),
        caller: parameters.get("callerId").cloned().unwrap_or_default(),
        timeout: parameters
            .get("timeout")
            .map(|value| value.parse())
            .transpose()
            .map_err(|_| ControlError::Invalid("Invalid timeout"))?
            .unwrap_or(30),
    })
}

fn create_bridge(
    id: ChannelId,
    parameters: &BTreeMap<String, String>,
) -> Result<Action, ControlError> {
    if let Some(types) = parameters.get("type") {
        if types
            .split(',')
            .any(|kind| !matches!(kind, "" | "mixing" | "proxy_media"))
        {
            return Err(ControlError::Unsupported);
        }
    }
    Ok(Action::CreateBridge {
        id,
        name: parameters.get("name").cloned().unwrap_or_default(),
    })
}

fn mute_direction(parameters: &BTreeMap<String, String>) -> Result<MuteDirection, ControlError> {
    match parameters
        .get("direction")
        .map(String::as_str)
        .unwrap_or("both")
    {
        "in" => Ok(MuteDirection::Incoming),
        "out" => Ok(MuteDirection::Outgoing),
        "both" => Ok(MuteDirection::Both),
        _ => Err(ControlError::Invalid("Invalid direction specified")),
    }
}

fn digit_timing(parameters: &BTreeMap<String, String>) -> Result<DtmfTiming, ControlError> {
    let value = |name, default| {
        parameters
            .get(name)
            .map(|value| value.parse::<u32>())
            .transpose()
            .map_err(|_| ControlError::Invalid("Invalid digit timing"))
            .map(|value| value.unwrap_or(default))
    };
    Ok(DtmfTiming::new(
        value("before", 0)?,
        value("between", 100)?,
        value("duration", 100)?,
        value("after", 0)?,
    )?)
}

fn response(outcome: Outcome) -> Result<Response, ControlError> {
    let status = StatusCode::from_u16(outcome.status)
        .map_err(|error| ControlError::Encoding(error.to_string()))?;
    let mut response = match outcome.body {
        Some(body) => (status, Json(body)).into_response(),
        None => status.into_response(),
    };
    if let Some(location) = outcome.location {
        response.headers_mut().insert(
            axum::http::header::LOCATION,
            location
                .parse()
                .map_err(|error: axum::http::header::InvalidHeaderValue| {
                    ControlError::Encoding(error.to_string())
                })?,
        );
    }
    Ok(response)
}

fn playback_media(parameters: &BTreeMap<String, String>) -> Result<MediaUri, ControlError> {
    MediaUri::try_from(required(parameters, "media")?).map_err(|error| match error {
        InvalidMediaUri::Unsupported => ControlError::Unsupported,
        InvalidMediaUri::Syntax | InvalidMediaUri::Name => {
            ControlError::Invalid("Invalid media URI")
        }
    })
}

fn playback_language(
    parameters: &BTreeMap<String, String>,
) -> Result<Option<ChannelId>, ControlError> {
    parameters
        .get("lang")
        .filter(|language| !language.is_empty())
        .map(|language| id(language))
        .transpose()
}

fn playback_options(
    parameters: &BTreeMap<String, String>,
) -> Result<rusterisk_api::PlaybackOptions, ControlError> {
    let milliseconds = |name: &str| {
        parameters
            .get(name)
            .filter(|value| !value.is_empty())
            .map(|value| {
                value
                    .parse::<u32>()
                    .map_err(|_| ControlError::Invalid("Invalid playback timing"))
                    .and_then(|value| {
                        rusterisk_api::PlaybackMilliseconds::try_from(value)
                            .map_err(|_| ControlError::Invalid("Invalid playback timing"))
                    })
            })
            .transpose()
            .map(Option::unwrap_or_default)
    };
    Ok(rusterisk_api::PlaybackOptions {
        offset_ms: milliseconds("offsetms")?,
        skip_ms: milliseconds("skipms")?,
    })
}

fn playback_control(
    parameters: &BTreeMap<String, String>,
) -> Result<PlaybackControl, ControlError> {
    match required(parameters, "operation")? {
        "pause" => Ok(PlaybackControl::Pause),
        "unpause" => Ok(PlaybackControl::Resume),
        "stop" => Ok(PlaybackControl::Stop),
        "restart" => Ok(PlaybackControl::Restart),
        "forward" => Ok(PlaybackControl::Forward),
        "reverse" => Ok(PlaybackControl::Reverse),
        _ => Err(ControlError::Invalid("Invalid playback operation")),
    }
}

fn documentation(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    scheme: HttpScheme,
) -> Result<Option<Response>, ControlError> {
    if method != Method::GET {
        return Ok(None);
    }
    let location = uri
        .path()
        .strip_prefix("/ari/api-docs/")
        .map(|path| (rusterisk_api::DiscoveryFormat::Asterisk, path))
        .or_else(|| {
            uri.path()
                .strip_prefix("/rusterisk/api-docs/")
                .map(|path| (rusterisk_api::DiscoveryFormat::Standard, path))
        });
    let Some((format, resource)) = location.and_then(|(format, path)| {
        path.strip_suffix(".json")
            .map(|resource| (format, resource))
    }) else {
        return Ok(None);
    };
    let mut document = if resource == "resources" {
        rusterisk_api::resources_for(format)
    } else {
        if !rusterisk_api::OPERATIONS
            .iter()
            .any(|operation| operation.resource() == resource)
        {
            return Err(ControlError::NotFound("Resource not found"));
        }
        rusterisk_api::swagger_for(resource, format)
            .map_err(|error| ControlError::Encoding(error.to_string()))?
    };
    let authority = match uri.authority() {
        Some(authority) => authority.as_str(),
        None => headers
            .get("Host")
            .ok_or(ControlError::Invalid("Missing request authority"))?
            .to_str()
            .map_err(|_| ControlError::Invalid("Invalid request authority"))?,
    };
    if authority.len() > 512 || authority.contains('@') {
        return Err(ControlError::Invalid("Invalid request authority"));
    }
    let authority: axum::http::uri::Authority = authority
        .parse()
        .map_err(|_| ControlError::Invalid("Invalid request authority"))?;
    if authority.host().is_empty() || (authority.port().is_some() && authority.port_u16().is_none())
    {
        return Err(ControlError::Invalid("Invalid request authority"));
    }
    let scheme = match scheme {
        HttpScheme::Http => "http",
        HttpScheme::Https => "https",
    };
    let base = if resource == "resources" && format == rusterisk_api::DiscoveryFormat::Standard {
        "/rusterisk"
    } else {
        "/ari"
    };
    document["basePath"] = Value::String(format!("{scheme}://{authority}{base}"));
    Ok(Some(Json(document).into_response()))
}

async fn dispatch(
    controller: &Controller,
    credentials: Credentials,
    origin: Option<String>,
    method: Method,
    uri: &Uri,
    parameters: &BTreeMap<String, String>,
    body: &[u8],
) -> Result<Response, ControlError> {
    let path = uri.path();
    let decoded = path
        .split('/')
        .skip(1)
        .map(|part| percent_encoding::percent_decode_str(part).decode_utf8())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ControlError::Invalid("Invalid path encoding"))?;
    let segments: Vec<&str> = decoded.iter().map(|part| part.as_ref()).collect();
    let operation = if let ["ari", resource @ ..] = segments.as_slice() {
        if let Some(slashless) = path.strip_suffix('/') {
            return Ok((
                StatusCode::NOT_FOUND,
                Json(rusterisk_api::ApiError {
                    message: format!("ARI URLs do not end with a slash. Try {slashless}"),
                }),
            )
                .into_response());
        }
        match rusterisk_api::ari_route(method.as_str(), resource.iter().copied()) {
            rusterisk_api::AriRoute::Declared(operation) => Some(operation),
            rusterisk_api::AriRoute::Unsupported => return Err(ControlError::Unsupported),
            rusterisk_api::AriRoute::NotFound => {
                return Err(ControlError::NotFound("Resource not found"))
            }
            rusterisk_api::AriRoute::OtherMethods(allow) => {
                let mut response = if method == Method::OPTIONS {
                    StatusCode::NO_CONTENT.into_response()
                } else {
                    (
                        StatusCode::METHOD_NOT_ALLOWED,
                        Json(rusterisk_api::ApiError {
                            message: "Invalid method".into(),
                        }),
                    )
                        .into_response()
                };
                response.headers_mut().insert(
                    axum::http::header::ALLOW,
                    allow
                        .parse()
                        .map_err(|error: axum::http::header::InvalidHeaderValue| {
                            ControlError::Encoding(error.to_string())
                        })?,
                );
                return Ok(response);
            }
        }
    } else {
        None
    };
    if method == Method::GET && segments == ["rusterisk", "openapi.json"] {
        return Ok(Json(rusterisk_api::openapi()).into_response());
    }
    if method == Method::GET && segments == ["ari", "asterisk", "ping"] {
        return Ok(Json(rusterisk_api::AsteriskPing {
            ping: "pong".into(),
            timestamp: crate::utc_timestamps::now_default(),
            asterisk_id: "rusterisk".into(),
        })
        .into_response());
    }
    if method == Method::GET && segments == ["ari", "asterisk", "info"] {
        return Ok(Json(
            controller
                .server_info(parameters.get("only").map(String::as_str))
                .await?,
        )
        .into_response());
    }
    if method == Method::GET && segments == ["rusterisk", "capabilities"] {
        let operations: Vec<Value> = rusterisk_api::OPERATIONS
            .iter()
            .map(|operation| json!({"operation": operation.id, "call_verified": false}))
            .collect();
        return Ok(Json(
            json!({"reference": rusterisk_api::REFERENCE_VERSION, "operations": operations}),
        )
        .into_response());
    }
    if method == Method::GET {
        let sound_query = match segments.as_slice() {
            ["ari", "sounds"] => Some(crate::runner::SoundQuery::List {
                language: parameters.get("lang").cloned(),
                format: parameters.get("format").cloned(),
            }),
            ["ari", "sounds", name] => Some(crate::runner::SoundQuery::Get(
                rusterisk_api::SoundId::try_from((*name).to_owned())
                    .map_err(|_| ControlError::Invalid("Invalid sound name"))?,
            )),
            _ => None,
        };
        if let Some(query) = sound_query {
            return response(controller.sounds(credentials, origin, query).await?);
        }
    }
    if let ("PUT", ["ari", "applications", application, "eventFilter"]) =
        (method.as_str(), segments.as_slice())
    {
        let update = if body.is_empty() {
            None
        } else {
            let value: Value = serde_json::from_slice(body)
                .map_err(|_| ControlError::Invalid("Invalid format definition"))?;
            let object = value
                .as_object()
                .ok_or(ControlError::Invalid("Invalid format definition"))?;
            if object.is_empty() {
                None
            } else {
                Some(
                    serde_json::from_value(value)
                        .map_err(|_| ControlError::Invalid("Invalid format definition"))?,
                )
            }
        };
        return response(
            controller
                .execute(
                    credentials,
                    origin,
                    Action::FilterApplication {
                        application: id(application)?.to_string(),
                        update,
                    },
                )
                .await?,
        );
    }
    let fields = request_body::decode(parameters, operation, body)?;
    let parameters = fields.parameters.as_ref();
    let stored = match (method.as_str(), segments.as_slice()) {
        ("GET", ["ari", "recordings", "stored"]) => Some(crate::StoredRecordingOperation::List),
        ("GET", ["ari", "recordings", "stored", name]) => {
            Some(crate::StoredRecordingOperation::Get(id(name)?))
        }
        ("GET", ["ari", "recordings", "stored", name, "file"]) => {
            Some(crate::StoredRecordingOperation::File(id(name)?))
        }
        ("POST", ["ari", "recordings", "stored", name, "copy"]) => {
            Some(crate::StoredRecordingOperation::Copy {
                source: id(name)?,
                destination: id(required(parameters, "destinationRecordingName")?)?,
            })
        }
        ("DELETE", ["ari", "recordings", "stored", name]) => {
            Some(crate::StoredRecordingOperation::Delete(id(name)?))
        }
        _ => None,
    };
    if let Some(operation) = stored {
        return match controller
            .stored_recording(credentials, origin, operation)
            .await?
        {
            crate::StoredRecordingResponse::Json(outcome) => response(outcome),
            crate::StoredRecordingResponse::File(file) => {
                let length = file.length();
                Response::builder()
                    .header(axum::http::header::CONTENT_TYPE, "audio/wav")
                    .header(axum::http::header::CONTENT_LENGTH, length)
                    .body(Body::from_stream(file.into_stream()))
                    .map_err(|error| ControlError::Encoding(error.to_string()))
            }
        };
    }
    let action = match (method.as_str(), segments.as_slice()) {
        ("POST", ["ari", "channels", "externalMedia"]) => external_media::action(parameters)?,
        ("GET", ["ari", "applications"]) => Action::ListApplications,
        ("GET", ["ari", "applications", application]) => {
            Action::GetApplication(id(application)?.to_string())
        }
        ("POST", ["ari", "applications", application, "subscription"]) => {
            Action::SubscribeResources {
                application: id(application)?.to_string(),
                sources: parameters
                    .get("eventSource")
                    .filter(|value| !value.is_empty())
                    .ok_or(ControlError::Invalid("Missing parameter eventSource"))?
                    .split(',')
                    .map(str::to_owned)
                    .collect(),
            }
        }
        ("DELETE", ["ari", "applications", application, "subscription"]) => {
            Action::UnsubscribeResources {
                application: id(application)?.to_string(),
                sources: parameters
                    .get("eventSource")
                    .filter(|value| !value.is_empty())
                    .ok_or(ControlError::Invalid("Missing parameter eventSource"))?
                    .split(',')
                    .map(str::to_owned)
                    .collect(),
            }
        }
        ("GET", ["ari", "endpoints"]) => Action::ListEndpoints { technology: None },
        ("GET", ["ari", "endpoints", technology]) => Action::ListEndpoints {
            technology: Some(id(technology)?.to_string()),
        },
        ("GET", ["ari", "endpoints", technology, resource]) => Action::GetEndpoint {
            technology: id(technology)?.to_string(),
            resource: id(resource)?.to_string(),
        },
        ("POST", ["ari", "channels", channel, "record"]) => {
            let arguments = recording::arguments(parameters)?;
            Action::RecordChannel {
                channel: id(channel)?,
                name: arguments.name,
                options: arguments.options,
            }
        }
        ("POST", ["ari", "bridges", bridge, "record"]) => {
            let arguments = recording::arguments(parameters)?;
            Action::RecordBridge {
                bridge: id(bridge)?,
                name: arguments.name,
                options: arguments.options,
            }
        }
        ("GET", ["ari", "recordings", "live", name]) => Action::GetRecording(id(name)?),
        ("DELETE", ["ari", "recordings", "live", name]) => Action::EndRecording {
            name: id(name)?,
            mode: RecordingEnd::Cancel,
        },
        ("POST", ["ari", "recordings", "live", name, "stop"]) => Action::EndRecording {
            name: id(name)?,
            mode: RecordingEnd::Finish,
        },
        ("POST", ["ari", "recordings", "live", name, "pause"]) => Action::ControlRecording {
            name: id(name)?,
            operation: RecordingControl::Pause,
        },
        ("DELETE", ["ari", "recordings", "live", name, "pause"]) => Action::ControlRecording {
            name: id(name)?,
            operation: RecordingControl::Resume,
        },
        ("POST", ["ari", "recordings", "live", name, "mute"]) => Action::ControlRecording {
            name: id(name)?,
            operation: RecordingControl::Mute,
        },
        ("DELETE", ["ari", "recordings", "live", name, "mute"]) => Action::ControlRecording {
            name: id(name)?,
            operation: RecordingControl::Unmute,
        },
        ("POST", ["ari", "channels", channel, "play"]) => Action::PlayChannel {
            channel: id(channel)?,
            id: optional_id(parameters, "playbackId")?,
            media: playback_media(parameters)?,
            language: playback_language(parameters)?,
            options: playback_options(parameters)?,
        },
        ("POST", ["ari", "channels", channel, "play", playback]) => Action::PlayChannel {
            channel: id(channel)?,
            id: id(playback)?,
            media: playback_media(parameters)?,
            language: playback_language(parameters)?,
            options: playback_options(parameters)?,
        },
        ("POST", ["ari", "bridges", bridge, "play"]) => Action::PlayBridge {
            bridge: id(bridge)?,
            id: optional_id(parameters, "playbackId")?,
            media: playback_media(parameters)?,
            language: playback_language(parameters)?,
            options: playback_options(parameters)?,
        },
        ("POST", ["ari", "bridges", bridge, "play", playback]) => Action::PlayBridge {
            bridge: id(bridge)?,
            id: id(playback)?,
            media: playback_media(parameters)?,
            language: playback_language(parameters)?,
            options: playback_options(parameters)?,
        },
        ("GET", ["ari", "playbacks", playback]) => Action::GetPlayback(id(playback)?),
        ("DELETE", ["ari", "playbacks", playback]) => Action::ControlPlayback {
            id: id(playback)?,
            operation: PlaybackControl::Stop,
        },
        ("POST", ["ari", "playbacks", playback, "control"]) => Action::ControlPlayback {
            id: id(playback)?,
            operation: playback_control(parameters)?,
        },
        ("GET", ["ari", "channels"]) => Action::ListChannels,
        ("POST", ["ari", "channels"]) => {
            originate(parameters, optional_id(parameters, "channelId")?)?
        }
        ("POST", ["ari", "channels", channel])
            if operation.is_some_and(|operation| operation.id == "channels.originateWithId") =>
        {
            originate(parameters, id(channel)?)?
        }
        ("GET", ["ari", "channels", channel]) => Action::GetChannel(id(channel)?),
        ("POST", ["ari", "channels", channel, "answer"]) => Action::Answer(id(channel)?),
        ("POST", ["ari", "channels", channel, "ring"]) => Action::Ring(id(channel)?),
        ("POST", ["ari", "channels", channel, "continue"]) => Action::Continue {
            channel: id(channel)?,
            context: parameters.get("context").cloned(),
            extension: parameters.get("extension").cloned(),
            priority: parameters
                .get("priority")
                .map(|value| value.parse())
                .transpose()
                .map_err(|_| ControlError::Invalid("Invalid dialplan priority"))?,
        },
        ("POST", ["ari", "channels", channel, "move"]) => Action::Move {
            channel: id(channel)?,
            app: required(parameters, "app")?.into(),
            args: parameters
                .get("appArgs")
                .map(|value| {
                    value
                        .split(',')
                        .filter(|value| !value.is_empty())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default(),
        },
        ("POST", ["ari", "channels", channel, "dtmf"]) => Action::SendDtmf {
            channel: id(channel)?,
            digits: required(parameters, "dtmf")?.into(),
            timing: digit_timing(parameters)?,
        },
        ("GET", ["ari", "channels", channel, "variable"]) => Action::GetVariable {
            channel: id(channel)?,
            variable: parameters
                .get("variable")
                .filter(|value| !value.is_empty())
                .ok_or(ControlError::Invalid("Variable name is required"))?
                .clone(),
        },
        ("POST", ["ari", "channels", channel, "variable"]) => Action::SetVariable {
            channel: id(channel)?,
            variable: required(parameters, "variable")?.into(),
            value: parameters.get("value").cloned(),
        },
        ("POST", ["ari", "channels", channel, "hold"]) => Action::Hold(id(channel)?),
        ("DELETE", ["ari", "channels", channel, "hold"]) => Action::Unhold(id(channel)?),
        ("POST", ["ari", "channels", channel, "moh"]) => Action::StartMusic {
            channel: id(channel)?,
            class: parameters.get("mohClass").cloned(),
        },
        ("DELETE", ["ari", "channels", channel, "moh"]) => Action::StopMusic(id(channel)?),
        ("POST", ["ari", "channels", channel, "mute"]) => Action::Mute {
            channel: id(channel)?,
            direction: mute_direction(parameters)?,
        },
        ("DELETE", ["ari", "channels", channel, "mute"]) => Action::Unmute {
            channel: id(channel)?,
            direction: mute_direction(parameters)?,
        },
        ("DELETE", ["ari", "channels", channel]) => Action::Hangup(id(channel)?),
        ("GET", ["ari", "bridges"]) => Action::ListBridges,
        ("POST", ["ari", "bridges"]) => {
            create_bridge(optional_id(parameters, "bridgeId")?, parameters)?
        }
        ("GET", ["ari", "bridges", bridge]) => Action::GetBridge(id(bridge)?),
        ("POST", ["ari", "bridges", bridge]) => create_bridge(id(bridge)?, parameters)?,
        ("DELETE", ["ari", "bridges", bridge]) => Action::DestroyBridge(id(bridge)?),
        ("POST", ["ari", "bridges", bridge, "addChannel"]) => Action::AddChannels {
            bridge: id(bridge)?,
            channels: required(parameters, "channel")?
                .split(',')
                .map(id)
                .collect::<Result<Vec<_>, _>>()?,
        },
        ("POST", ["ari", "bridges", bridge, "removeChannel"]) => Action::RemoveChannels {
            bridge: id(bridge)?,
            channels: required(parameters, "channel")?
                .split(',')
                .map(id)
                .collect::<Result<Vec<_>, _>>()?,
        },
        _ if operation.is_some() => return Err(ControlError::Unsupported),
        _ => return Err(ControlError::NotFound("Resource not found")),
    };
    if let Action::Answer(channel) = action {
        return response(
            controller
                .answer_channel(credentials, origin, channel)
                .await?,
        );
    }
    response(
        controller
            .execute_with_variables(credentials, origin, action, fields.variables)
            .await?,
    )
}

async fn api(
    controller: Controller,
    scheme: HttpScheme,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let result = async {
        let parameters = parameters(&uri)?;
        let credentials = credentials(&headers, &parameters)?;
        let origin = headers
            .get("Origin")
            .map(|value| value.to_str().map(String::from))
            .transpose()
            .map_err(|_| ControlError::Forbidden)?;
        controller
            .authorize(
                credentials.clone(),
                origin.clone(),
                method == Method::GET || method == Method::OPTIONS,
            )
            .await?;
        if let Some(response) = documentation(&method, &uri, &headers, scheme)? {
            return Ok(response);
        }
        dispatch(
            &controller,
            credentials,
            origin,
            method,
            &uri,
            &parameters,
            &body,
        )
        .await
    }
    .await;
    match result {
        Ok(response) => response,
        Err(failure) => error(failure),
    }
}
