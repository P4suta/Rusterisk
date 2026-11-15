use super::optional_id;
use crate::{Action, ControlError};
use rusterisk_api::{AudioSocketTarget, ExternalRtpTarget, StaticRtpCodec};
use std::{collections::BTreeMap, net::SocketAddr};

fn value<'a>(parameters: &'a BTreeMap<String, String>, name: &str, default: &'a str) -> &'a str {
    parameters
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
}

pub(super) fn action(parameters: &BTreeMap<String, String>) -> Result<Action, ControlError> {
    let app = value(parameters, "app", "");
    if app.is_empty() {
        return Err(ControlError::Invalid("app cannot be empty"));
    }
    let transport = value(parameters, "transport", "udp");
    let encapsulation = value(parameters, "encapsulation", "rtp");
    if transport == "websocket" && encapsulation != "none" {
        return Err(ControlError::Invalid(
            "encapsulation must be 'none' for websocket transport",
        ));
    }
    if encapsulation == "rtp" && transport != "udp" {
        return Err(ControlError::Invalid(
            "transport must be 'udp' for rtp encapsulation",
        ));
    }
    if encapsulation == "audiosocket" && transport != "tcp" {
        return Err(ControlError::Invalid(
            "transport must be 'tcp' for audiosocket encapsulation",
        ));
    }
    let connection = value(parameters, "connection_type", "client");
    if transport != "websocket" && connection == "server" {
        return Err(ControlError::Invalid(
            "'server' connection_type can only be used with the websocket transport",
        ));
    }
    let host = value(parameters, "external_host", "");
    if host.is_empty() && !(transport == "websocket" && connection == "server") {
        return Err(ControlError::Invalid(
            "external_host is required for all but websocket server connections",
        ));
    }
    let format = value(parameters, "format", "");
    if format.is_empty() {
        return Err(ControlError::Invalid("format cannot be empty"));
    }
    let direction = value(parameters, "direction", "both");
    if !matches!(direction, "both" | "in" | "out") {
        return Err(ControlError::Invalid("Invalid direction specified"));
    }
    if transport == "websocket" && encapsulation == "none" && connection == "server" {
        if !host.is_empty() && host != "INCOMING" {
            return Err(ControlError::Unsupported);
        }
        let format = match format {
            "ulaw" => rusterisk_api::MediaFormat::Pcmu,
            "alaw" => rusterisk_api::MediaFormat::Pcma,
            "slin" => rusterisk_api::MediaFormat::Pcm8,
            "slin12" => rusterisk_api::MediaFormat::Pcm12,
            "slin16" => rusterisk_api::MediaFormat::Pcm16,
            "slin24" => rusterisk_api::MediaFormat::Pcm24,
            "slin32" => rusterisk_api::MediaFormat::Pcm32,
            "slin44" => rusterisk_api::MediaFormat::Pcm44,
            "slin48" => rusterisk_api::MediaFormat::Pcm48,
            _ => return Err(ControlError::Unsupported),
        };
        let mut control_format = None;
        let mut auto_answer = true;
        let mut options = value(parameters, "transport_data", "");
        while !options.is_empty() {
            if let Some(tail) = options.strip_prefix('n') {
                auto_answer = false;
                options = tail;
            } else if let Some(tail) = options.strip_prefix("f(") {
                let (format, tail) = tail
                    .split_once(')')
                    .ok_or(ControlError::Invalid("Invalid media options"))?;
                control_format = Some(match format {
                    "json" => rusterisk_api::MediaControlFormat::Json,
                    "plain-text" => rusterisk_api::MediaControlFormat::PlainText,
                    _ => return Err(ControlError::Invalid("Invalid media control format")),
                });
                options = tail;
            } else {
                return Err(ControlError::Unsupported);
            }
        }
        let direction = match direction {
            "in" => rusterisk_api::MediaDirection::In,
            "out" => rusterisk_api::MediaDirection::Out,
            "both" => rusterisk_api::MediaDirection::Both,
            _ => return Err(ControlError::Invalid("Invalid direction specified")),
        };
        return Ok(Action::ExternalWebSocket {
            id: optional_id(parameters, "channelId")?,
            app: app.into(),
            connection_id: rusterisk_api::ChannelId::try_from(uuid::Uuid::new_v4().to_string())?,
            format,
            control_format,
            direction,
            auto_answer,
            data: value(parameters, "data", "").into(),
        });
    }
    if !matches!(
        (transport, encapsulation),
        ("udp", "rtp") | ("tcp", "audiosocket")
    ) || connection != "client"
        || direction != "both"
        || !value(parameters, "transport_data", "").is_empty()
    {
        return Err(ControlError::Unsupported);
    }
    let remote: SocketAddr = match host.parse() {
        Ok(remote) => remote,
        Err(_) => {
            let authority = host.parse::<axum::http::uri::Authority>().map_err(|_| {
                ControlError::Invalid(
                    "external_host must be <host>:<port> for all transports other than websocket",
                )
            })?;
            if host.contains('@')
                || authority.host().is_empty()
                || authority.port_u16().is_none_or(|port| port == 0)
            {
                return Err(ControlError::Invalid(
                    "external_host must be <host>:<port> for all transports other than websocket",
                ));
            }
            return Err(ControlError::Unsupported);
        }
    };
    if transport == "tcp" {
        let data = value(parameters, "data", "");
        if data.is_empty() {
            return Err(ControlError::Invalid("data can not be empty"));
        }
        let session = uuid::Uuid::parse_str(data).map_err(|_| {
            ControlError::Adapter(
                "An internal error prevented this request from being handled".into(),
            )
        })?;
        if format != "slin" {
            return Err(ControlError::Unsupported);
        }
        let target = AudioSocketTarget::new(remote, *session.as_bytes())
            .map_err(|_| ControlError::Invalid("Invalid external media address"))?;
        return Ok(Action::ExternalAudioSocket {
            id: optional_id(parameters, "channelId")?,
            app: app.into(),
            target,
        });
    }
    let codec = match format {
        "ulaw" => StaticRtpCodec::Pcmu,
        "alaw" => StaticRtpCodec::Pcma,
        _ => return Err(ControlError::Unsupported),
    };
    let target = ExternalRtpTarget::new(remote, codec)
        .map_err(|_| ControlError::Invalid("Invalid external media address"))?;
    Ok(Action::ExternalRtp {
        id: optional_id(parameters, "channelId")?,
        app: app.into(),
        target,
        data: value(parameters, "data", "").into(),
    })
}
