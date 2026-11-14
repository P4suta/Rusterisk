use crate::{Action, ControlError, Controller, Credentials, Outcome};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{any, get},
    Json, Router,
};
use base64::Engine;
use rusterisk_api::ChannelId;
use rusterisk_core::{DtmfTiming, MuteDirection};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn router(controller: Controller) -> Router {
    router_for_scheme(controller, HttpScheme::Http)
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
        .route("/ari/events", get(crate::websocket::upgrade))
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

fn response(outcome: Outcome) -> Response {
    match outcome.body {
        Some(body) => (StatusCode::OK, Json(body)).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
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
    if method == Method::GET && path == "/rusterisk/openapi.json" {
        return Ok(Json(rusterisk_api::openapi()).into_response());
    }
    if method == Method::GET && path == "/ari/asterisk/ping" {
        return Ok(Json(rusterisk_api::AsteriskPing {
            ping: "pong".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            asterisk_id: "rusterisk".into(),
        })
        .into_response());
    }
    if method == Method::GET && path == "/rusterisk/capabilities" {
        let operations: Vec<Value> = rusterisk_api::OPERATIONS
            .iter()
            .map(|operation| json!({"operation": operation.id, "call_verified": false}))
            .collect();
        return Ok(Json(
            json!({"reference": rusterisk_api::REFERENCE_VERSION, "operations": operations}),
        )
        .into_response());
    }
    if !body.is_empty() {
        let value: Value =
            serde_json::from_slice(body).map_err(|_| ControlError::Invalid("Invalid JSON body"))?;
        if !value.is_object() {
            return Err(ControlError::Invalid("Expected a JSON object"));
        }
        return Err(ControlError::Unsupported);
    }
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let action = match (method.as_str(), segments.as_slice()) {
        ("GET", ["ari", "channels"]) => Action::ListChannels,
        ("POST", ["ari", "channels"]) => Action::Originate {
            id: optional_id(parameters, "channelId")?,
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
        },
        ("GET", ["ari", "channels", channel]) => Action::GetChannel(id(channel)?),
        ("POST", ["ari", "channels", channel, "answer"]) => Action::Answer(id(channel)?),
        ("POST", ["ari", "channels", channel, "ring"]) => Action::Ring(id(channel)?),
        ("POST", ["ari", "channels", channel, "dtmf"]) => Action::SendDtmf {
            channel: id(channel)?,
            digits: required(parameters, "dtmf")?.into(),
            timing: digit_timing(parameters)?,
        },
        ("GET", ["ari", "channels", channel, "variable"]) => Action::GetVariable {
            channel: id(channel)?,
            variable: required(parameters, "variable")?.into(),
        },
        ("POST", ["ari", "channels", channel, "variable"]) => Action::SetVariable {
            channel: id(channel)?,
            variable: required(parameters, "variable")?.into(),
            value: parameters.get("value").cloned(),
        },
        ("POST", ["ari", "channels", channel, "hold"]) => Action::Hold(id(channel)?),
        ("DELETE", ["ari", "channels", channel, "hold"]) => Action::Unhold(id(channel)?),
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
        ("POST", ["ari", "bridges"]) => Action::CreateBridge {
            id: optional_id(parameters, "bridgeId")?,
            name: parameters.get("name").cloned().unwrap_or_default(),
        },
        ("GET", ["ari", "bridges", bridge]) => Action::GetBridge(id(bridge)?),
        ("POST", ["ari", "bridges", bridge]) => Action::CreateBridge {
            id: id(bridge)?,
            name: parameters.get("name").cloned().unwrap_or_default(),
        },
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
        (_, ["ari", ..]) => return Err(ControlError::Unsupported),
        _ => return Err(ControlError::NotFound("Resource not found")),
    };
    Ok(response(
        controller.execute(credentials, origin, action).await?,
    ))
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
            .authorize(credentials.clone(), origin.clone(), method == Method::GET)
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
