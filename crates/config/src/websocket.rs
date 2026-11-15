use crate::{Config, ConfigError, Ini, Secret, Section};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WebSocketControlFormat {
    #[default]
    PlainText,
    Json,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WebSocketConfig {
    pub control_format: WebSocketControlFormat,
    pub clients: Vec<WebSocketClient>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebSocketClient {
    pub name: String,
    pub uri: Secret,
    pub username: Option<Secret>,
    pub password: Option<Secret>,
    pub ca_list_file: Option<String>,
    pub connection_timeout_ms: u16,
    pub reconnect_interval_ms: u16,
    pub reconnect_attempts: u16,
}

impl WebSocketClient {
    pub fn requires_tls(&self) -> bool {
        self.uri.expose().starts_with("wss:")
    }
}

fn client(section: Section) -> Result<WebSocketClient, ConfigError> {
    let file = "websocket_client.conf";
    if section.name.contains('/') || matches!(section.name.as_str(), "." | "..") {
        return Err(ConfigError::at(
            file,
            section.line,
            "invalid media connection id",
        ));
    }
    let error = |key: &str, reason: &str| ConfigError::at(file, section.position(key), reason);
    section.check(
        file,
        &[
            "type",
            "connection_type",
            "uri",
            "protocols",
            "username",
            "password",
            "ca_list_file",
            "connection_timeout",
            "reconnect_interval",
            "reconnect_attempts",
            "tls_enabled",
            "verify_server_cert",
            "verify_server_hostname",
        ],
    )?;
    if section.required(file, "type")? != "websocket_client" {
        return Err(error("type", "expected websocket_client"));
    }
    if section.required(file, "connection_type")? != "per_call_config" {
        return Err(error(
            "connection_type",
            "media clients require per_call_config",
        ));
    }
    if section.required(file, "protocols")? != "media" {
        return Err(error(
            "protocols",
            "media clients require the media protocol",
        ));
    }
    let uri = section.required(file, "uri")?;
    let url = url::Url::parse(uri).map_err(|_| error("uri", "invalid media URI"))?;
    if uri.len() > 2048
        || !matches!(url.scheme(), "ws" | "wss")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(error("uri", "invalid media URI"));
    }
    if section.flag(file, "tls_enabled", false)? != (url.scheme() == "wss") {
        return Err(error("tls_enabled", "TLS must match the URI scheme"));
    }
    for key in ["verify_server_cert", "verify_server_hostname"] {
        if !section.flag(file, key, true)? {
            return Err(error(key, "media clients require server verification"));
        }
    }
    let ca_list_file = section.value("ca_list_file");
    if ca_list_file.is_some_and(|path| {
        url.scheme() != "wss"
            || path.is_empty()
            || path.len() > 4096
            || path.chars().any(char::is_control)
    }) {
        return Err(error("ca_list_file", "invalid TLS trust file"));
    }
    let username = section.value("username");
    let password = section.value("password");
    if username.is_some() != password.is_some() {
        return Err(error(
            if password.is_some() {
                "password"
            } else {
                "username"
            },
            "username and password must be paired",
        ));
    }
    if username.is_some_and(|name| {
        name.is_empty()
            || name.len() > 128
            || name.contains(':')
            || name.chars().any(char::is_control)
    }) {
        return Err(error("username", "invalid media username"));
    }
    if password.is_some_and(|password| {
        password.is_empty() || password.len() > 256 || password.chars().any(char::is_control)
    }) {
        return Err(error("password", "invalid media password"));
    }
    let connection_timeout_ms = section.number(file, "connection_timeout", 500)?;
    let reconnect_interval_ms = section.number(file, "reconnect_interval", 500)?;
    let reconnect_attempts = section.number(file, "reconnect_attempts", 4)?;
    if connection_timeout_ms == 0 || connection_timeout_ms > 4000 {
        return Err(error(
            "connection_timeout",
            "connection_timeout must be between 1 and 4000 milliseconds",
        ));
    }
    if reconnect_interval_ms == 0 || reconnect_interval_ms > 4000 {
        return Err(error(
            "reconnect_interval",
            "reconnect_interval must be between 1 and 4000 milliseconds",
        ));
    }
    if !(1..=4).contains(&reconnect_attempts) {
        return Err(error(
            "reconnect_attempts",
            "reconnect_attempts must be between 1 and 4",
        ));
    }
    let total = u32::from(connection_timeout_ms) * u32::from(reconnect_attempts)
        + u32::from(reconnect_interval_ms) * u32::from(reconnect_attempts - 1);
    if total > 4000 {
        return Err(error(
            "connection_timeout",
            "media connection attempts exceed the four-second admission budget",
        ));
    }
    Ok(WebSocketClient {
        name: section.name.clone(),
        uri: Secret(url.to_string()),
        username: username.map(|value| Secret(value.into())),
        password: password.map(|value| Secret(value.into())),
        ca_list_file: ca_list_file.map(str::to_owned),
        connection_timeout_ms,
        reconnect_interval_ms,
        reconnect_attempts,
    })
}

impl Config {
    pub fn with_websocket_configuration(
        mut self,
        channel: &str,
        clients: &str,
    ) -> Result<Self, ConfigError> {
        let mut config = WebSocketConfig::default();
        for section in Ini::parse("chan_websocket.conf", channel)?.sections {
            if section.name != "global" {
                return Err(ConfigError::at(
                    "chan_websocket.conf",
                    section.line,
                    "expected global section",
                ));
            }
            section.check("chan_websocket.conf", &["control_message_format"])?;
            config.control_format = match section
                .value("control_message_format")
                .unwrap_or("plain-text")
            {
                "plain-text" => WebSocketControlFormat::PlainText,
                "json" => WebSocketControlFormat::Json,
                _ => {
                    return Err(ConfigError::at(
                        "chan_websocket.conf",
                        section.position("control_message_format"),
                        "invalid control_message_format",
                    ))
                }
            };
        }
        let sections = Ini::parse("websocket_client.conf", clients)?.sections;
        if sections.len() > 64 {
            return Err(ConfigError::at(
                "websocket_client.conf",
                1,
                "media client capacity exceeded",
            ));
        }
        for section in sections {
            config.clients.push(client(section)?);
        }
        self.websocket = config;
        Ok(self)
    }
}
