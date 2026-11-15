use super::optional_id;
use crate::{Action, ControlError};
use rusterisk_api::{ExternalRtpTarget, StaticRtpCodec};
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
    if transport != "udp"
        || encapsulation != "rtp"
        || connection != "client"
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
