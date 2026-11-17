use crate::{valid_name, valid_origin, Config, ConfigError, RegistrationUriError, MAX_SECTIONS};
use std::collections::BTreeSet;

mod dialplan;
mod signaling;

const MAX_VALUE_BYTES: usize = 4096;

fn invalid(file: &str, reason: impl Into<String>) -> ConfigError {
    ConfigError::at(file, 0, reason)
}

fn value(file: &str, field: &str, value: &str) -> Result<(), ConfigError> {
    if value.len() > MAX_VALUE_BYTES {
        Err(invalid(file, format!("{field} exceeds its byte capacity")))
    } else {
        Ok(())
    }
}

fn required(file: &str, field: &str, text: &str) -> Result<(), ConfigError> {
    value(file, field, text)?;
    if text.is_empty() {
        Err(invalid(file, format!("missing {field}")))
    } else {
        Ok(())
    }
}

fn names<'a>(
    file: &str,
    values: impl Iterator<Item = &'a str>,
    capacity: usize,
) -> Result<(), ConfigError> {
    let mut seen = BTreeSet::new();
    for (index, name) in values.enumerate() {
        if index >= capacity || !valid_name(name) || !seen.insert(name) {
            return Err(invalid(file, "invalid object name, duplicate, or capacity"));
        }
    }
    Ok(())
}

fn list<'a>(file: &str, values: impl Iterator<Item = &'a str>) -> Result<(), ConfigError> {
    let mut bytes = 0usize;
    for (index, text) in values.enumerate() {
        value(file, "list entry", text)?;
        bytes = bytes
            .checked_add(text.len())
            .filter(|total| *total <= MAX_VALUE_BYTES)
            .ok_or_else(|| invalid(file, "list exceeds its byte capacity"))?;
        if index >= MAX_VALUE_BYTES {
            return Err(invalid(file, "list exceeds its entry capacity"));
        }
    }
    Ok(())
}

pub(super) fn valid_rtp_range(start: u16, end: u16) -> bool {
    start >= 1024 && start.is_multiple_of(2) && end > start
}

pub(super) fn valid_dial_destinations(destinations: &[String]) -> bool {
    !destinations.is_empty()
        && destinations.len() <= 16
        && destinations
            .iter()
            .all(|value| value.starts_with("PJSIP/") && value.len() > 6)
}

pub(super) fn static_contact(
    config: &Config,
    name: &str,
    source: &str,
) -> Result<(), RegistrationUriError> {
    let target = crate::registration_uri::ContactTarget::parse(source)?;
    for endpoint in config
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.aors.iter().any(|aor| aor == name))
    {
        let transport = config
            .transports
            .iter()
            .find(|transport| transport.name == endpoint.transport)
            .ok_or(RegistrationUriError::TransportConflict)?;
        target.for_transport(transport.protocol)?;
    }
    Ok(())
}

fn access(config: &Config) -> Result<(), ConfigError> {
    if !config.http.bind.ip().is_loopback() && config.http.tls.is_none() {
        return Err(invalid("http.conf", "public management requires TLS"));
    }
    if let Some(files) = &config.http.tls {
        required("http.conf", "tlscertfile", &files.certificate)?;
        required("http.conf", "tlsprivatekey", &files.private_key)?;
    }
    if config.users.is_empty() {
        return Err(invalid("ari.conf", "at least one user is required"));
    }
    names(
        "ari.conf",
        config.users.iter().map(|user| user.name.as_str()),
        MAX_SECTIONS,
    )?;
    for user in &config.users {
        required("ari.conf", "password", user.password.expose())?;
    }
    list("ari.conf", config.origins.iter().map(String::as_str))?;
    if config.origins.iter().any(|origin| !valid_origin(origin)) {
        return Err(invalid(
            "ari.conf",
            "allowed_origins requires exact secure or loopback origins",
        ));
    }
    Ok(())
}

fn media(config: &Config) -> Result<(), ConfigError> {
    let rtp = &config.rtp;
    if (rtp.start, rtp.end) != (0, 0) && !valid_rtp_range(rtp.start, rtp.end) {
        return Err(invalid("rtp.conf", "invalid runtime RTP port range"));
    }
    for text in [&rtp.stun, &rtp.turn, &rtp.turn_username]
        .into_iter()
        .flatten()
    {
        value("rtp.conf", "media profile", text)?;
    }
    if let Some(password) = &rtp.turn_password {
        value("rtp.conf", "turnpassword", password.expose())?;
    }
    Ok(())
}

impl Config {
    /// Checks typed runtime values and references without reading external resources.
    /// Accepts ephemeral listeners and a pair of zero media ports.
    pub fn validate_runtime(&self) -> Result<(), ConfigError> {
        access(self)?;
        signaling::validate(self)?;
        self.websocket.validate_runtime()?;
        media(self)?;
        dialplan::validate(&self.dialplan)
    }
}
