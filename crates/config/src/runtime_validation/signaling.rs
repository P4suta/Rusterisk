use super::{invalid, list, names, required, static_contact, value, MAX_VALUE_BYTES};
use crate::{
    Aor, Auth, Config, ConfigError, Endpoint, Protocol, Registration, Transport, MAX_REGISTRATIONS,
    MAX_SECTIONS, MAX_TRANSPORTS,
};

const FILE: &str = "pjsip.conf";

fn transport(config: &Config, transport: &Transport) -> Result<(), ConfigError> {
    match transport.protocol {
        Protocol::Tls => {
            let files = transport
                .tls
                .as_ref()
                .ok_or_else(|| invalid(FILE, "TLS transport requires certificate and key"))?;
            required(FILE, "cert_file", &files.certificate)?;
            required(FILE, "priv_key_file", &files.private_key)?;
            if let Some(path) = &transport.ca_list_file {
                required(FILE, "ca_list_file", path)?;
            }
        }
        Protocol::Udp | Protocol::Tcp | Protocol::Ws | Protocol::Wss => {
            if transport.tls.is_some() || transport.ca_list_file.is_some() {
                return Err(invalid(FILE, "TLS material requires a TLS transport"));
            }
        }
    }
    if matches!(transport.protocol, Protocol::Ws | Protocol::Wss) {
        if (transport.protocol == Protocol::Wss) != config.http.tls.is_some() {
            return Err(invalid(
                FILE,
                "WebSocket transport conflicts with HTTP encryption",
            ));
        }
        if config.http.bind.port() != 0 && transport.bind != config.http.bind {
            return Err(invalid(
                FILE,
                "WebSocket transport conflicts with the HTTP listener",
            ));
        }
    }
    Ok(())
}

fn credentials(auth: &Auth) -> Result<(), ConfigError> {
    required(FILE, "username", &auth.username)?;
    required(FILE, "password", auth.password.expose())?;
    value(FILE, "realm", &auth.realm)
}

fn address(config: &Config, aor: &Aor) -> Result<(), ConfigError> {
    if aor.max_contacts > 16 {
        return Err(invalid(FILE, "max_contacts must be 0 through 16"));
    }
    if aor.minimum_expiration > aor.maximum_expiration {
        return Err(invalid(
            FILE,
            "minimum_expiration must not exceed maximum_expiration",
        ));
    }
    if let Some(contact) = &aor.contact {
        static_contact(config, &aor.name, contact)
            .map_err(|error| invalid(FILE, format!("invalid static Contact: {error}")))?;
    }
    Ok(())
}

fn endpoint(endpoint: &Endpoint) -> Result<(), ConfigError> {
    if endpoint.codecs.is_empty() || endpoint.codecs.len() > MAX_VALUE_BYTES {
        return Err(invalid(
            FILE,
            "endpoint requires a bounded nonempty codec list",
        ));
    }
    if endpoint.aors.is_empty() || endpoint.outbound_auth.len() > 8 {
        return Err(invalid(FILE, "invalid endpoint address or credential list"));
    }
    list(FILE, endpoint.aors.iter().map(String::as_str))?;
    list(FILE, endpoint.outbound_auth.iter().map(String::as_str))?;
    value(FILE, "context", &endpoint.context)
}

fn registration(config: &Config, registration: &Registration) -> Result<(), ConfigError> {
    let transport = config
        .transports
        .iter()
        .find(|transport| transport.name == registration.transport)
        .ok_or_else(|| invalid(FILE, "unresolved registration transport"))?;
    crate::registration::validate_roles(
        transport.protocol,
        &registration.server_uri,
        &registration.client_uri,
    )
    .map_err(|issue| invalid(FILE, issue.reason()))?;
    if !crate::registration::valid_contact_user(&registration.contact_user) {
        return Err(invalid(
            FILE,
            "contact_user requires a bounded ASCII SIP URI user",
        ));
    }
    Ok(())
}

pub(super) fn validate(config: &Config) -> Result<(), ConfigError> {
    let count = [
        config.transports.len(),
        config.auth.len(),
        config.aors.len(),
        config.endpoints.len(),
        config.registrations.len(),
        config.identify.len(),
    ]
    .into_iter()
    .try_fold(0usize, |total, count| total.checked_add(count))
    .ok_or_else(|| invalid(FILE, "signaling object capacity exceeded"))?;
    if count > MAX_SECTIONS {
        return Err(invalid(FILE, "signaling object capacity exceeded"));
    }
    names(
        FILE,
        config.transports.iter().map(|value| value.name.as_str()),
        MAX_TRANSPORTS,
    )?;
    names(
        FILE,
        config.auth.iter().map(|value| value.name.as_str()),
        MAX_SECTIONS,
    )?;
    names(
        FILE,
        config.aors.iter().map(|value| value.name.as_str()),
        MAX_SECTIONS,
    )?;
    names(
        FILE,
        config.endpoints.iter().map(|value| value.name.as_str()),
        MAX_SECTIONS,
    )?;
    names(
        FILE,
        config.registrations.iter().map(|value| value.name.as_str()),
        MAX_REGISTRATIONS,
    )?;
    for protocol in [Protocol::Ws, Protocol::Wss] {
        if config
            .transports
            .iter()
            .filter(|transport| transport.protocol == protocol)
            .count()
            > 1
        {
            return Err(invalid(FILE, "ambiguous WebSocket transport metadata"));
        }
    }
    for settings in &config.transports {
        transport(config, settings)?;
    }
    for auth in &config.auth {
        credentials(auth)?;
    }
    for settings in &config.endpoints {
        endpoint(settings)?;
    }
    config.validate_references()?;
    for aor in &config.aors {
        address(config, aor)?;
    }
    for settings in &config.registrations {
        registration(config, settings)?;
    }
    Ok(())
}
