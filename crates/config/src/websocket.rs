use crate::{Config, ConfigError, Ini, Secret, Section};
mod validation;

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
    validation::name(&section.name)
        .map_err(|error| ConfigError::at(file, section.line, error.reason))?;
    let error = |key: &str, reason: &str| ConfigError::at(file, section.position(key), reason);
    let invalid = |issue: validation::InvalidClient| error(issue.field, issue.reason);
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
    let url = validation::uri(uri).map_err(invalid)?;
    if uri.len() > 2048 {
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
    validation::ca(ca_list_file, url.scheme() == "wss").map_err(invalid)?;
    let username = section.value("username");
    let password = section.value("password");
    validation::credentials(username, password).map_err(invalid)?;
    let connection_timeout_ms = section.number(file, "connection_timeout", 500)?;
    let reconnect_interval_ms = section.number(file, "reconnect_interval", 500)?;
    let reconnect_attempts = section.number(file, "reconnect_attempts", 4)?;
    validation::timing(
        connection_timeout_ms,
        reconnect_interval_ms,
        reconnect_attempts,
    )
    .map_err(invalid)?;
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

impl WebSocketConfig {
    pub(super) fn validate_runtime(&self) -> Result<(), ConfigError> {
        let file = "websocket_client.conf";
        let invalid = |issue: validation::InvalidClient| ConfigError::at(file, 0, issue.reason);
        if self.clients.len() > 64 {
            return Err(ConfigError::at(file, 0, "media client capacity exceeded"));
        }
        let mut names = std::collections::BTreeSet::new();
        for client in &self.clients {
            validation::name(&client.name).map_err(invalid)?;
            if !names.insert(client.name.as_str()) {
                return Err(ConfigError::at(file, 0, "duplicate media connection id"));
            }
            let uri = validation::uri(client.uri.expose()).map_err(invalid)?;
            if uri.as_str() != client.uri.expose() {
                return Err(ConfigError::at(
                    file,
                    0,
                    "media URI requires normalized spelling",
                ));
            }
            validation::ca(client.ca_list_file.as_deref(), uri.scheme() == "wss")
                .map_err(invalid)?;
            validation::credentials(
                client.username.as_ref().map(Secret::expose),
                client.password.as_ref().map(Secret::expose),
            )
            .map_err(invalid)?;
            validation::timing(
                client.connection_timeout_ms,
                client.reconnect_interval_ms,
                client.reconnect_attempts,
            )
            .map_err(invalid)?;
        }
        Ok(())
    }
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
